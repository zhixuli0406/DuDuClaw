//! CDP page controls, kept separate from transport and native approval policy.
use super::cdp_pipe::CdpPipe;
use serde_json::{Value, json};

pub(crate) fn allowed_url(raw: &str, hosts: &[String], initial_blank: bool) -> bool {
    if initial_blank && raw == "about:blank" {
        return true;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url
            .host_str()
            .is_some_and(|host| hosts.iter().any(|allowed| allowed == host))
}

pub(crate) async fn install(
    pipe: &CdpPipe,
    session: &str,
    width: u32,
    height: u32,
) -> Result<(), String> {
    pipe.call("Page.enable", json!({}), Some(session)).await?;
    pipe.call("Runtime.enable", json!({}), Some(session))
        .await?;
    pipe.call(
        "Page.setInterceptFileChooserDialog",
        json!({"enabled":true}),
        Some(session),
    )
    .await?;
    pipe.call(
        "Fetch.enable",
        json!({"handleAuthRequests":true,"patterns":[{"urlPattern":"*"}]}),
        Some(session),
    )
    .await?;
    pipe.call(
        "Emulation.setDeviceMetricsOverride",
        json!({"width":width,"height":height,"deviceScaleFactor":1,"mobile":false}),
        Some(session),
    )
    .await?;
    Ok(())
}

pub(crate) async fn frame(
    pipe: &CdpPipe,
    session: &str,
    hosts: &[String],
) -> Result<String, String> {
    let tree = pipe
        .call("Page.getFrameTree", json!({}), Some(session))
        .await?;
    let frame = &tree["frameTree"]["frame"];
    let url = frame["url"].as_str().ok_or("page URL unavailable")?;
    if !allowed_url(url, hosts, true) {
        return Err("local_navigation_refused".into());
    }
    frame["id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "page frame unavailable".into())
}

/// Isolated world prevents page scripts from replacing the globals this probe
/// uses. It does not turn a potentially misleading DOM into trusted content.
pub(crate) async fn evaluate(
    pipe: &CdpPipe,
    session: &str,
    hosts: &[String],
    expression: &str,
) -> Result<Value, String> {
    let frame = frame(pipe, session, hosts).await?;
    let world = pipe.call("Page.createIsolatedWorld",json!({"frameId":frame,"worldName":"DuDuClaw safety observer","grantUniveralAccess":false}),Some(session)).await?;
    let context = world["executionContextId"]
        .as_i64()
        .ok_or("isolated world unavailable")?;
    let answer = pipe.call("Runtime.evaluate",json!({"expression":expression,"contextId":context,"returnByValue":true,"awaitPromise":false,"silent":true}),Some(session)).await?;
    if answer.get("exceptionDetails").is_some() {
        return Err("page observation unavailable".into());
    }
    answer["result"]
        .get("value")
        .cloned()
        .ok_or_else(|| "page observation unavailable".into())
}

pub(crate) const FOCUS_SAFE: &str = r#"(() => {
  let element = document.activeElement;
  for (let depth=0; depth<12; depth++) {
    if (!element) return false;
    if (element.tagName === 'IFRAME' || element.tagName === 'FRAME') {
      try { element = element.contentDocument.activeElement; } catch (_) { return false; }
    } else if (element.shadowRoot && element.shadowRoot.activeElement) {
      element = element.shadowRoot.activeElement;
    } else break;
  }
  if (!element || element.tagName.includes('-')) return false;
  const type=(element.getAttribute('type')||'').toLowerCase();
  const autocomplete=(element.getAttribute('autocomplete')||'').toLowerCase();
  return !['password','file','hidden'].includes(type) &&
    !/(password|one-time-code|cc-)/.test(autocomplete);
})()"#;

pub(crate) const MASK_REGIONS: &str = r#"(() => {
  const rectangles=[];
  function add(element) {
    for (const box of element.getClientRects()) {
      const x=Math.max(0,Math.floor(box.left)-2), y=Math.max(0,Math.floor(box.top)-2);
      const width=Math.max(0,Math.ceil(box.right)+2-x), height=Math.max(0,Math.ceil(box.bottom)+2-y);
      if (width && height) rectangles.push([x,y,width,height]);
      if (rectangles.length>1000) throw new Error('mask limit');
    }
  }
  function visit(root) {
    for (const element of root.querySelectorAll('*')) {
      // Every form value, inaccessible frame and custom-element rectangle is
      // conservative: closed shadow trees cannot be inspected reliably.
      if (element.matches('input,textarea,iframe,frame,[contenteditable],.credit-card,[data-sensitive]') || element.tagName.includes('-')) add(element);
      if (element.shadowRoot) visit(element.shadowRoot);
    }
  }
  visit(document); return rectangles;
})()"#;

pub(crate) fn key(raw: &str) -> Option<(&'static str, &'static str, u32)> {
    Some(match raw {
        "Return" | "Enter" => ("Enter", "Enter", 13),
        "Tab" => ("Tab", "Tab", 9),
        "Escape" | "Esc" => ("Escape", "Escape", 27),
        "BackSpace" | "Backspace" => ("Backspace", "Backspace", 8),
        "Delete" => ("Delete", "Delete", 46),
        "Left" | "ArrowLeft" => ("ArrowLeft", "ArrowLeft", 37),
        "Right" | "ArrowRight" => ("ArrowRight", "ArrowRight", 39),
        "Up" | "ArrowUp" => ("ArrowUp", "ArrowUp", 38),
        "Down" | "ArrowDown" => ("ArrowDown", "ArrowDown", 40),
        "Home" => ("Home", "Home", 36),
        "End" => ("End", "End", 35),
        "Page_Up" | "PageUp" => ("PageUp", "PageUp", 33),
        "Page_Down" | "PageDown" => ("PageDown", "PageDown", 34),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_refuses_every_non_https_scheme_and_authority_escape() {
        let hosts = vec!["example.com".into()];
        assert!(allowed_url("https://example.com/path", &hosts, false));
        assert!(allowed_url("about:blank", &hosts, true));
        for url in [
            "file:///synthetic/sentinel",
            "data:text/html,synthetic",
            "chrome://version/",
            "javascript:alert(1)",
            "http://example.com",
            "https://example.com:444",
            "https://other.example.com/",
            "https://user:pass@example.com/",
            "https://127.0.0.1/",
            "about:blank",
        ] {
            assert!(!allowed_url(url, &hosts, false), "{url}");
        }
    }
    #[test]
    fn browser_accelerators_clipboard_and_password_typing_shortcuts_are_not_keys() {
        for value in [
            "ctrl+o",
            "ctrl+l",
            "ctrl+p",
            "ctrl+v",
            "meta+v",
            "super+v",
            "F12",
            "Print",
            "a",
            "file:///sentinel",
        ] {
            assert!(key(value).is_none(), "{value}");
        }
        assert_eq!(key("Return"), Some(("Enter", "Enter", 13)));
    }
}
