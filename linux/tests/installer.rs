use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use drv_i18n::{Catalog, Msg};

const LANGUAGES: [&str; 2] = ["en", "ru"];
const SCRIPTS: [&str; 2] = ["install.sh", "uninstall.sh"];
const HELPERS: [&str; 2] = ["messages.sh", "trust.sh"];
const SPEAKERS: [&str; 4] = ["say", "item", "fail", "text"];
const FAMILIES: [&str; 2] = ["setup-", "remove-"];

fn dist() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("dist")
}

fn source(language: &str) -> String {
    std::fs::read_to_string(dist().join("locales").join(format!("{language}.ftl"))).unwrap()
}

fn calls() -> Vec<(String, BTreeSet<String>)> {
    let mut found = Vec::new();
    for script in SCRIPTS {
        let text = std::fs::read_to_string(dist().join(script)).unwrap();
        for line in text.lines() {
            let words: Vec<&str> = line.split_whitespace().collect();
            for (at, word) in words.iter().enumerate() {
                let Some(key) = words.get(at + 1).filter(|key| SPEAKERS.contains(word) && FAMILIES.iter().any(|family| key.starts_with(family))) else {
                    continue;
                };
                let names = words[at + 2..].iter().map_while(|word| word.split_once('=').map(|(name, _)| name.to_string())).collect();
                found.push((key.to_string(), names));
            }
        }
    }
    found
}

fn shell(script: &str, arguments: &[String]) -> String {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(". \"$1/messages.sh\"; catalogs=$1/locales; shift; {script}"))
        .arg("sh")
        .arg(dist())
        .args(arguments)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim_end_matches('\n').to_string()
}

#[test]
fn installer_catalogues_agree_with_each_other_and_with_the_scripts() {
    let reference: BTreeSet<String> = drv_i18n::keys(&source("en")).into_iter().collect();
    let mut used: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (key, names) in calls() {
        assert!(reference.contains(&key), "{key} is not in the catalogue");
        let expected: BTreeSet<String> = drv_i18n::variables(&source("en"), &key).into_iter().collect();
        assert_eq!(names, expected, "arguments passed with {key}");
        used.insert(key, names);
    }
    let unused: Vec<&String> = reference.iter().filter(|key| !used.contains_key(*key)).collect();
    assert!(unused.is_empty(), "never printed: {unused:?}");
    for language in LANGUAGES {
        let text = source(language);
        let present: BTreeSet<String> = drv_i18n::keys(&text).into_iter().collect();
        assert_eq!(present, reference, "{language}: key set differs from en");
        for line in text.lines().filter(|line| !line.is_empty()) {
            let (key, message) = line.split_once(" = ").unwrap_or_else(|| panic!("{language}: not a one-line message: {line}"));
            assert!(reference.contains(key), "{language}: {line}");
            assert_eq!(drv_i18n::variables(&text, key), drv_i18n::variables(&source("en"), key), "{language}: variables of {key}");
            let plain = message.replace("{ $", "").replace(" }", "");
            assert!(!plain.contains(['{', '}']) && !message.starts_with(' ') && !message.ends_with(' '), "{language}: {key}");
        }
    }
}

#[test]
fn the_shell_renders_installer_messages_exactly_as_the_library_does() {
    for language in LANGUAGES {
        let text = source(language);
        let catalog = Catalog::for_languages(&[(language, text.as_str())], &[language.to_string()]);
        assert!(catalog.is_loaded() && catalog.language() == language);
        for key in drv_i18n::keys(&text) {
            let names = drv_i18n::variables(&text, &key);
            let value = |name: &str| format!("/{name} & \\1 50% $HOME `x` a=b");
            let message = names.iter().fold(Msg::new(&key), |message, name| message.arg(name, value(name)));
            let mut arguments = vec![key.clone()];
            arguments.extend(names.iter().map(|name| format!("{name}={}", value(name))));
            let rendered = shell(&format!("language={language}; text \"$@\""), &arguments);
            assert_eq!(rendered, catalog.try_render(&message).unwrap_or_else(|missing| panic!("{language}: {key}: {missing:?}")), "{language}: {key}");
        }
    }
}

#[test]
fn every_installer_script_parses_in_the_shells_at_hand() {
    let shells: Vec<Vec<&str>> = vec![vec!["sh"], vec!["dash"], vec!["busybox", "sh"]];
    let mut checked = 0;
    for shell in shells {
        if Command::new(shell[0]).arg("-c").arg("exit 0").status().is_err() {
            continue;
        }
        for script in SCRIPTS.iter().chain(HELPERS.iter()).map(|name| dist().join(name)).chain([
            dist().join("openrc/elantpd"),
            dist().join("runit/run"),
            dist().join("s6/run"),
        ]) {
            let parsed = Command::new(shell[0]).args(&shell[1..]).arg("-n").arg(&script).output().unwrap();
            assert!(parsed.status.success(), "{shell:?} {}: {}", script.display(), String::from_utf8_lossy(&parsed.stderr));
            checked += 1;
        }
    }
    assert!(checked >= SCRIPTS.len() + HELPERS.len());
}

#[test]
fn every_service_definition_starts_the_installed_service() {
    for file in ["systemd/elantpd.service", "openrc/elantpd", "runit/run", "dinit/elantpd", "s6/run"] {
        let text = std::fs::read_to_string(dist().join(file)).unwrap();
        assert!(text.contains("/usr/local/bin/elantpd"), "{file}");
    }
    let unit = std::fs::read_to_string(dist().join("systemd/elantpd.service")).unwrap();
    for line in ["NoNewPrivileges=yes", "CapabilityBoundingSet=", "DevicePolicy=closed", "PrivateNetwork=yes", "ProtectSystem=strict"] {
        assert!(unit.lines().any(|unit_line| unit_line == line), "{line}");
    }
}
