pub trait ResultExt {
    fn ignore(self);
}

impl<T, E> ResultExt for Result<T, E> {
    fn ignore(self) {
        let _ = self;
    }
}
