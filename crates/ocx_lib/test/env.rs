pub fn get_lock() -> impl Drop {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap()
}

macro_rules! lock {
    () => {
        let _test_env_lock = crate::test::env::get_lock();
    };
}
pub(crate) use lock;
