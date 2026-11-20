pub mod config;
pub mod evdev;
pub mod hidraw;
pub mod keys;
pub mod recording;
pub mod session;
pub mod uinput;

pub const LOCALES: [(&str, &str); 2] = [("ru", include_str!("../../locales/ru.ftl")), ("en", include_str!("../../locales/en.ftl"))];

pub fn catalog() -> drv_i18n::Catalog {
    drv_i18n::Catalog::new(&LOCALES)
}

pub fn notice(gestures: &gestus::Gestures, notice: gestus::Notice) -> drv_i18n::Msg {
    use gestus::Notice;
    match notice {
        Notice::Touchpad(true) => drv_i18n::Msg::new("gesture-touchpad-on"),
        Notice::Touchpad(false) => drv_i18n::Msg::new("gesture-touchpad-off"),
        Notice::Numpad(true) => drv_i18n::Msg::new("gesture-numpad-on"),
        Notice::Numpad(false) => drv_i18n::Msg::new("gesture-numpad-off"),
        Notice::Tablet(true) => drv_i18n::Msg::new("gesture-tablet-on"),
        Notice::Tablet(false) => drv_i18n::Msg::new("gesture-tablet-off"),
        Notice::Drawing => drv_i18n::Msg::new("gesture-drawing"),
        Notice::Symbol(index) => drv_i18n::Msg::new("gesture-symbol").arg("name", gestures.symbol(index).unwrap_or_default()),
        Notice::Unrecognized => drv_i18n::Msg::new("gesture-unrecognized"),
        Notice::Cancelled => drv_i18n::Msg::new("gesture-cancelled"),
    }
}
