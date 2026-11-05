//! The installers pass `HASHI_CONNECT` to `hashi connect` only when it has
//! exactly the shape the claim page builds. These tests keep their patterns
//! in step with `SAFE_ORIGIN` and `SAFE_CODE` in `src/web/hashi.js`, and run
//! the `sh` check itself. Nothing is installed or downloaded.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

const INSTALL_SH: &str = include_str!("../install.sh");
const INSTALL_PS1: &str = include_str!("../install.ps1");
const HASHI_JS: &str = include_str!("../src/web/hashi.js");

/// The body of the JS regex literal `const <name> = /^...$/;`.
fn js_regex(name: &str) -> String {
    let line = HASHI_JS
        .lines()
        .find(|l| l.starts_with(&format!("const {name} = /")))
        .unwrap();
    let body = line
        .split_once("= /^")
        .unwrap()
        .1
        .rsplit_once("$/;")
        .unwrap()
        .0;
    body.replace("\\/", "/")
}

/// The single-quoted value assigned to `var` on its own line.
fn quoted(text: &str, var: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&format!("{var} = '")) || l.starts_with(&format!("{var}='")))
        .unwrap_or_else(|| panic!("{var} not found"));
    let start = line.find('\'').unwrap() + 1;
    let end = line.rfind('\'').unwrap();
    line[start..end].to_owned()
}

#[test]
fn installer_patterns_match_the_claim_page() {
    let link = format!(
        "{}/claim/{}",
        js_regex("SAFE_ORIGIN"),
        js_regex("SAFE_CODE")
    );
    assert_eq!(
        quoted(INSTALL_PS1, "$connectPattern"),
        format!("^{link}\\z")
    );
    let ere = link.replace("(?:", "(");
    assert_eq!(quoted(INSTALL_SH, "CONNECT_PATTERN"), format!("^{ere}$"));
}

#[test]
fn installers_pass_the_link_after_a_double_dash() {
    assert!(INSTALL_SH.contains(r#""$INSTALL_DIR/hashi" connect -- "$HASHI_CONNECT""#));
    assert!(INSTALL_PS1.contains("@('connect', '--', $link)"));
    assert!(!INSTALL_PS1.contains("& $exe connect $link"));
}

#[cfg(unix)]
#[test]
fn the_sh_check_accepts_only_claim_page_links() {
    // The pattern and the function, as install.sh defines them.
    let start = INSTALL_SH.find("CONNECT_PATTERN=").unwrap();
    let end = start + INSTALL_SH[start..].find("\n}\n").unwrap() + 3;
    let defs = &INSTALL_SH[start..end];
    let check = |link: &str| {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{defs}\nvalid_connect_link \"$1\""))
            .arg("sh")
            .arg(link)
            .status()
            .unwrap()
            .success()
    };
    let code = format!("hashi_inv_{}", "0123456789abcdef".repeat(4));
    for good in [
        format!("https://bridge.example.com/claim/{code}"),
        format!("https://bridge.example.com:8443/claim/{code}"),
        format!("http://127.0.0.1:7878/claim/{code}"),
        format!("http://localhost/claim/{code}"),
        format!("https://[2001:db8::1]/claim/{code}"),
    ] {
        assert!(check(&good), "{good}");
    }
    for bad in [
        format!("https://bridge.example.com/claim/{code}'; touch /tmp/x; '"),
        format!("https://bridge.example.com/claim/{code}\nhttps://x.example/claim/{code}"),
        format!("x\nhttps://bridge.example.com/claim/{code}"),
        format!("http://bridge.example.com/claim/{code}"),
        format!("--setup=claude https://bridge.example.com/claim/{code}"),
        format!("https://user@bridge.example.com/claim/{code}"),
        format!("https://bridge.example.com/claim/{}", code.to_uppercase()),
        "https://bridge.example.com/claim/hashi_inv_0123".to_owned(),
        String::new(),
    ] {
        assert!(!check(&bad), "{bad:?}");
    }
}
