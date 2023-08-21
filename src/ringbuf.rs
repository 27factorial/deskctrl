// This is used in one place so I'm not worried about an exhaustive API
pub struct RingBuf<T> {
    slots: Box<[Option<T>]>,
    cursor: usize,
}

impl<T> RingBuf<T> {
    pub fn new(capacity: usize) -> Self {
        let slots = std::iter::repeat_with(|| None).take(capacity).collect();

        Self { slots, cursor: 0 }
    }

    pub fn push(&mut self, elem: T) -> Option<T> {
        let slot = &mut self.slots[self.cursor];

        let ret = std::mem::replace(slot, Some(elem));
        self.cursor = (self.cursor + 1) % self.slots.len();

        ret
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots
            .iter()
            .take_while(|&slot| slot.is_some())
            .map(|opt| opt.as_ref().unwrap())
    }
}
