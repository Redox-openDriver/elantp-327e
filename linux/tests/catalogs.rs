use std::collections::BTreeSet;

use drv_i18n::{Catalog, Msg};

const SOURCES: [&str; 7] = [
    include_str!("../src/bin/elantp-dev.rs"),
    include_str!("../src/bin/elantpd.rs"),
    include_str!("../src/lib.rs"),
    include_str!("../src/session.rs"),
    include_str!("../src/config.rs"),
    include_str!("../../redox/src/lib.rs"),
    include_str!("../../redox/src/bin/elantpd.rs"),
];

fn used() -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for source in SOURCES {
        for piece in source.split('"').skip(1).step_by(2) {
            if ["dev-", "session-", "redox-", "gesture-", "service-"].iter().any(|prefix| piece.starts_with(prefix))
                && !piece.ends_with('-')
                && piece.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                keys.insert(piece.to_string());
            }
        }
    }
    keys
}

#[test]
fn every_language_has_the_same_messages_with_the_same_variables() {
    let reference: BTreeSet<String> = drv_i18n::keys(elantp_linux::LOCALES[1].1).into_iter().collect();
    for (language, source) in elantp_linux::LOCALES {
        assert_eq!(drv_i18n::keys(source).into_iter().collect::<BTreeSet<_>>(), reference, "{language}");
        for key in &reference {
            assert_eq!(drv_i18n::variables(source, key), drv_i18n::variables(elantp_linux::LOCALES[1].1, key), "{language}: {key}");
        }
    }
}

#[test]
fn every_message_the_tool_names_exists_and_none_is_left_unused() {
    let reference: BTreeSet<String> = drv_i18n::keys(elantp_linux::LOCALES[1].1).into_iter().collect();
    assert_eq!(used(), reference);
}

#[test]
fn every_message_renders_in_every_language_with_its_own_variables() {
    for (language, source) in elantp_linux::LOCALES {
        let catalog = Catalog::for_languages(&elantp_linux::LOCALES, &[language.to_string()]);
        assert!(catalog.is_loaded(), "{language}");
        for key in drv_i18n::keys(source) {
            let message = drv_i18n::variables(source, &key).into_iter().fold(Msg::new(&key), |message, name| message.arg(&name, "x"));
            catalog.try_render(&message).unwrap_or_else(|problems| panic!("{language}: {problems:?}"));
        }
    }
}
