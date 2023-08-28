use anyhow::Context;
use gdk_pixbuf::{Colorspace, Pixbuf};
use glib::Bytes;
use hyphenation::{Language, Load, Standard as HyphenStandard};
use ring::digest;
use serde::{Deserialize, Serialize, Serializer};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use textwrap::{Options as WrapOptions, WordSplitter};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
};
use zvariant::{OwnedValue, Type, Value};

pub const NOTIFICATIONS_FILE: &str = "notifications.json";
pub const NOTIFICATION_LIMIT: usize = 32;
pub const LINE_LENGTH: usize = 36;

#[derive(Clone, PartialEq, Debug, Default, Deserialize, Type)]
pub struct DBusNotification {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
    expire_timeout: i32,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Serialize)]
pub struct EwwNotification {
    pub id: u32,
    pub timestamp: u64,
    pub app_name: String,
    // This is only used internally to keep track of which windows correspond to which notifications
    #[serde(skip)]
    pub app_class: String,
    pub summary: String,
    pub body: String,
    pub app_icon: Option<PathBuf>,
    // Putting this behind Arc instead of using PathBuf will make it much cheaper to copy
    // when caching the image count.
    #[serde(serialize_with = "serialize_opt_arc_path")]
    pub image_path: Option<Arc<Path>>,
    pub tmp_image: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug, Default)]
pub struct PixbufConfig {
    width: i32,
    height: i32,
    row_stride: i32,
    has_alpha: bool,
}

fn serialize_opt_arc_path<S: Serializer>(
    opt: &Option<Arc<Path>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    opt.as_ref().map(|path| path.as_ref()).serialize(serializer)
}

pub async fn make_eww(mut dbus_notif: DBusNotification) -> anyhow::Result<EwwNotification> {
    enum ImageKind {
        Path,
        Data,
    }

    // DBus notifications can either specify images using a path or inline data. If data is used,
    // the image-data structure looks like this:
    // int32 width
    // int32 height
    // int32 rowstride
    // boolean has_alpha
    // int32 bits_per_sample (always 8, don't need)
    // int32 channels (always 3 unless has_alpha is true, don't need)
    // byte[] data
    //
    // The DBus Notification spec also requires us to check for an image hints in the order:
    // image-data, image-path, icon_data (deprecated).
    // For completeness, after the official checks, this application also checks the following hints
    // in this order:
    // image_data, image_path

    let check_order = [
        ("image-data", ImageKind::Data),
        ("image-path", ImageKind::Path),
        ("icon_data", ImageKind::Data),
        ("image_data", ImageKind::Data),
        ("image_path", ImageKind::Path),
    ];

    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .context("Failed to get duration since Unix epoch")?
        .as_secs();

    let image_info = {
        // (image_path, tmp_image)
        let mut ret = None;

        for (key, kind) in check_order {
            let Some(value) = dbus_notif.hints.remove(key).map(Into::<Value>::into) else {
                continue;
            };

            match kind {
                ImageKind::Data => {
                    // Some processing is needed to convert the Pixbuf image into a PNG and save
                    // it somewhere on disk. For now, I'll use the /tmp/deskctrl/images directory.
                    let (width, height, row_stride, has_alpha, _, _, image_data) =
                        TryInto::<(i32, i32, i32, bool, i32, i32, Vec<u8>)>::try_into(value)
                            .context("Failed to deserialize image data to tuple (iiibiiay)")?;

                    let config = PixbufConfig {
                        width,
                        height,
                        row_stride,
                        has_alpha,
                    };

                    // DBus Notifications use the GDK Pixbuf format for image data. This function
                    // converts it to a PNG instead, which can be displayed by eww.
                    let image_bytes = convert_image(image_data, config)
                        .context("Failed to convert Pixbuf image to PNG")?;

                    let filename = get_digest(&image_bytes);

                    let path = PathBuf::from(format!("{}/{filename}.png", crate::IMAGE_PATH));

                    let mut image_file = create_dir_and_file(&path)
                        .await
                        .context("Failed to create file in IMAGE_PATH")?;

                    image_file
                        .write_all(&image_bytes)
                        .await
                        .context("Failed to write to image file")?;

                    ret = Some((path, true));
                    break;
                }
                ImageKind::Path => {
                    let path = PathBuf::from(
                        TryInto::<String>::try_into(value)
                            .context("Failed to convert image path to String (s)")?,
                    );

                    ret = Some((path, false));
                    break;
                }
            }
        }

        ret
    };

    let app_icon = if !dbus_notif.app_icon.is_empty() {
        Some(dbus_notif.app_icon.into())
    } else {
        None
    };

    let (image_path, tmp_image) = if let Some((image_path, tmp_image)) = image_info {
        (Some(image_path.into()), tmp_image)
    } else {
        (None, false)
    };

    // Note that this "app class" may not be *exactly* the same as the hyprland class, but
    // most applications tend to be compatible with this method.
    let app_class = dbus_notif.app_name.to_lowercase();

    // Capitalizes the first character of the app name, so it looks better when
    // displayed as a notification.
    let app_name = {
        let mut ret = dbus_notif.app_name;
        if let Some(char) = ret.chars().next() {
            ret = char.to_uppercase().chain(ret.chars().skip(1)).collect();
        }
        ret
    };

    let summary = word_wrap(&dbus_notif.summary, LINE_LENGTH)?;
    let body = word_wrap(&dbus_notif.body, LINE_LENGTH)?;

    Ok(EwwNotification {
        id: dbus_notif.replaces_id,
        timestamp,
        app_name,
        app_class,
        summary,
        body,
        app_icon,
        image_path,
        tmp_image,
    })
}

fn convert_image(image: Vec<u8>, config: PixbufConfig) -> anyhow::Result<Vec<u8>> {
    // I'm not entirely sure why there's a need to go through the `Bytes` wrapper, since
    // it doesn't seem to do any input validation.
    let bytes = Bytes::from_owned(image);
    let pixbuf = Pixbuf::from_bytes(
        &bytes,
        Colorspace::Rgb,
        config.has_alpha,
        8,
        config.width,
        config.height,
        config.row_stride,
    );

    pixbuf
        .save_to_bufferv("png", &[])
        .context("Failed to convert Pixbuf image to PNG")
}

pub async fn create_dir_and_file<P: AsRef<Path>>(path: P) -> anyhow::Result<File> {
    let path = path.as_ref();

    let directory = path
        .parent()
        .context("Failed to create file in path with no parent directory")?;

    fs::create_dir_all(directory)
        .await
        .context("Failed to create directory")?;

    let file = File::create(path)
        .await
        .context("Failed to create file in directory")?;

    Ok(file)
}

fn word_wrap(content: &str, line_length: usize) -> anyhow::Result<String> {
    let options = WrapOptions::new(line_length)
        .break_words(true)
        .word_splitter(WordSplitter::Hyphenation(HyphenStandard::from_embedded(
            Language::EnglishUS,
        )?));

    Ok(textwrap::fill(content, options))
}

fn get_digest(bytes: &[u8]) -> String {
    // A lookup table isn't strictly necessary, but does prevent allocating a new String every time
    // a byte is converted to hex to be displayed.
    const HEX_DIGITS: [&str; 256] = [
        "00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "0a", "0b", "0c", "0d", "0e",
        "0f", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "1a", "1b", "1c", "1d",
        "1e", "1f", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "2a", "2b", "2c",
        "2d", "2e", "2f", "30", "31", "32", "33", "34", "35", "36", "37", "38", "39", "3a", "3b",
        "3c", "3d", "3e", "3f", "40", "41", "42", "43", "44", "45", "46", "47", "48", "49", "4a",
        "4b", "4c", "4d", "4e", "4f", "50", "51", "52", "53", "54", "55", "56", "57", "58", "59",
        "5a", "5b", "5c", "5d", "5e", "5f", "60", "61", "62", "63", "64", "65", "66", "67", "68",
        "69", "6a", "6b", "6c", "6d", "6e", "6f", "70", "71", "72", "73", "74", "75", "76", "77",
        "78", "79", "7a", "7b", "7c", "7d", "7e", "7f", "80", "81", "82", "83", "84", "85", "86",
        "87", "88", "89", "8a", "8b", "8c", "8d", "8e", "8f", "90", "91", "92", "93", "94", "95",
        "96", "97", "98", "99", "9a", "9b", "9c", "9d", "9e", "9f", "a0", "a1", "a2", "a3", "a4",
        "a5", "a6", "a7", "a8", "a9", "aa", "ab", "ac", "ad", "ae", "af", "b0", "b1", "b2", "b3",
        "b4", "b5", "b6", "b7", "b8", "b9", "ba", "bb", "bc", "bd", "be", "bf", "c0", "c1", "c2",
        "c3", "c4", "c5", "c6", "c7", "c8", "c9", "ca", "cb", "cc", "cd", "ce", "cf", "d0", "d1",
        "d2", "d3", "d4", "d5", "d6", "d7", "d8", "d9", "da", "db", "dc", "dd", "de", "df", "e0",
        "e1", "e2", "e3", "e4", "e5", "e6", "e7", "e8", "e9", "ea", "eb", "ec", "ed", "ee", "ef",
        "f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "fa", "fb", "fc", "fd", "fe",
        "ff",
    ];

    let mut context = digest::Context::new(&digest::SHA256);
    context.update(bytes);
    let digest = context.finish();
    let mut hex_bytes = String::with_capacity(2 * digest.as_ref().len());

    digest
        .as_ref()
        .iter()
        .for_each(|&byte| hex_bytes.push_str(HEX_DIGITS[byte as usize]));
    hex_bytes
}
