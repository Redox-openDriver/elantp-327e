pub mod evdev;
pub mod hidraw;
pub mod recording;
pub mod session;
pub mod uinput;

pub const LOCALES: [(&str, &str); 2] = [("ru", include_str!("../../locales/ru.ftl")), ("en", include_str!("../../locales/en.ftl"))];

pub fn catalog() -> drv_i18n::Catalog {
    drv_i18n::Catalog::new(&LOCALES)
}
