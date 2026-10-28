//! Native companion. No webview command can supply a decision or presence.
//! Only the app's own child gateway is eligible for bootstrap and polling.
#[path = "../../crates/duduclaw-core/src/local_browser_channel/protocol.rs"]
mod wire;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Manager};

#[derive(Default)]
pub(crate) struct NativeControls {
    stop: AtomicBool,
    resume: AtomicBool,
    label: Mutex<String>,
    ready: AtomicBool,
    start_dialog: AtomicBool,
    start_request: Mutex<Option<NativeStartIntent>>,
    start_generation: AtomicU64,
}
struct NativeStartIntent {
    employee: String,
    generation: u64,
    expires: std::time::Instant,
}
impl NativeControls {
    fn invalidate_start(&self) {
        self.start_generation.fetch_add(1, Ordering::AcqRel);
        self.start_request
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.stop.store(true, Ordering::Release);
    }
    fn take_start_request(&self) -> Option<String> {
        let intent = self
            .start_request
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()?;
        (self.ready.load(Ordering::Acquire)
            && !self.stop.load(Ordering::Acquire)
            && intent.generation == self.start_generation.load(Ordering::Acquire)
            && std::time::Instant::now() < intent.expires)
            .then_some(intent.employee)
    }
}
pub(crate) fn emergency_stop(app: &AppHandle) {
    if let Some(state) = app.try_state::<NativeControls>() {
        state.invalidate_start();
    }
}
pub(crate) fn continue_from_tray(app: &AppHandle) {
    if let Some(state) = app.try_state::<NativeControls>() {
        state.resume.store(true, Ordering::Release);
    }
}
#[cfg(target_os = "macos")]
pub(crate) fn start_from_tray(app: &AppHandle) {
    mac::request_start(app);
}

pub(crate) fn start(
    app: AppHandle,
    manager: Arc<crate::sidecar::SidecarManager>,
    pid: u32,
    generation: u64,
    port: u16,
) {
    #[cfg(target_os = "macos")]
    tauri::async_runtime::spawn(async move {
        if let Err(why) = mac::run(app, manager, pid, generation, port).await {
            tracing::warn!(reason=%why,"local browser native companion unavailable");
        }
    });
    #[cfg(not(target_os = "macos"))]
    let _ = (app, manager, pid, generation, port);
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use base64::Engine;
    use serde_json::{json, Value};
    use std::{
        os::fd::AsRawFd,
        path::Path,
        time::{Duration, Instant},
    };
    use tokio::io::AsyncReadExt;
    use zeroize::Zeroizing;

    fn active_icon(icon: &tauri::image::Image<'_>) -> tauri::image::Image<'static> {
        let mut pixels = icon.rgba().to_vec();
        for pixel in pixels.chunks_exact_mut(4) {
            if pixel[3] != 0 {
                let luminance = (u32::from(pixel[0]) * 54
                    + u32::from(pixel[1]) * 183
                    + u32::from(pixel[2]) * 19)
                    / 256;
                pixel[0] = 255;
                pixel[1] = (80 + luminance * 140 / 255) as u8;
                pixel[2] = (luminance * 20 / 255) as u8;
            }
        }
        tauri::image::Image::new_owned(pixels, icon.width(), icon.height())
    }
    fn notice(app: &AppHandle, text: &'static str) {
        let _ = app.run_on_main_thread(move || {
            use objc2::MainThreadMarker;
            use objc2_app_kit::NSAlert;
            use objc2_foundation::NSString;
            if let Some(main) = MainThreadMarker::new() {
                let alert = NSAlert::new(main);
                alert.setMessageText(&NSString::from_str("本機專用瀏覽器"));
                alert.setInformativeText(&NSString::from_str(text));
                alert.runModal();
            }
        });
    }
    pub(super) fn request_start(app: &AppHandle) {
        let Some(state) = app.try_state::<NativeControls>() else {
            return;
        };
        if !state.ready.load(Ordering::Acquire) {
            notice(app,"本機原生通道尚未就緒。請使用這台 app 自己啟動的 gateway，確認功能已啟用、人仍在場，並從原生選單按繼續。遠端或附掛 gateway 不能使用。");
            return;
        }
        if state.start_dialog.swap(true, Ordering::AcqRel) {
            return;
        }
        struct Reset(AppHandle);
        impl Drop for Reset {
            fn drop(&mut self) {
                if let Some(state) = self.0.try_state::<NativeControls>() {
                    state.start_dialog.store(false, Ordering::Release);
                }
            }
        }
        let generation = state.start_generation.load(Ordering::Acquire);
        let reset = Reset(app.clone());
        let host = app.clone();
        let _=app.run_on_main_thread(move|| {
            let _reset=reset;
            use objc2::{MainThreadMarker,MainThreadOnly};use objc2_app_kit::{NSAlert,NSAlertFirstButtonReturn,NSTextField,NSApplication};use objc2_foundation::{NSString,NSRect,NSPoint,NSSize};
            let Some(main)=MainThreadMarker::new() else{return};
            #[allow(deprecated)]
            NSApplication::sharedApplication(main).activateIgnoringOtherApps(true);
            let alert=NSAlert::new(main);
            alert.setMessageText(&NSString::from_str("讓 AI 開啟本機專用瀏覽器"));
            alert.setInformativeText(&NSString::from_str("來源：本機原生使用者（平台身分未驗證）。這不代表員工負責人本人，也不建立聊天身分。輸入已由管理員啟用、且有有效負責人的員工代號；後續 AI 讀取與操作仍需要真人對話及本機核准。"));
            let input=NSTextField::initWithFrame(NSTextField::alloc(main),NSRect::new(NSPoint::new(0.0,0.0),NSSize::new(360.0,28.0)));
            input.setPlaceholderString(Some(&NSString::from_str("員工代號，例如 alice")));alert.setAccessoryView(Some(&input));
            alert.addButtonWithTitle(&NSString::from_str("取消"));alert.addButtonWithTitle(&NSString::from_str("讓 AI 開瀏覽器"));
            if alert.runModal()==NSAlertFirstButtonReturn+1 {
                let employee=input.stringValue().to_string().trim().to_owned();
                let state=host.state::<NativeControls>();
                if state.ready.load(Ordering::Acquire) && state.start_generation.load(Ordering::Acquire)==generation && !state.stop.load(Ordering::Acquire) && !employee.is_empty() && employee.chars().count()<=64 && !employee.chars().any(char::is_control) {
                    *state.start_request.lock().unwrap_or_else(|p|p.into_inner())=Some(NativeStartIntent{employee,generation,expires:Instant::now()+Duration::from_secs(60)});
                }
            }
        });
    }

    #[derive(serde::Serialize)]
    struct Presence {
        session_available: bool,
        on_console: bool,
        login_done: bool,
        locked: bool,
        idle_seconds: f64,
        browser_frontmost: bool,
        input_sequence: u64,
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGSessionCopyCurrentDictionary() -> core_foundation::dictionary::CFDictionaryRef;
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event: u32) -> f64;
        fn CGEventSourceCounterForEventType(state: i32, event: u32) -> u32;
    }
    fn presence(pid: u32) -> Presence {
        use core_foundation::{
            base::{CFType, TCFType},
            boolean::CFBoolean,
            dictionary::CFDictionary,
            string::CFString,
        };
        let raw = unsafe { CGSessionCopyCurrentDictionary() };
        let dictionary = if raw.is_null() {
            None
        } else {
            Some(unsafe { CFDictionary::<CFString, CFType>::wrap_under_create_rule(raw) })
        };
        let boolean = |key: &str| {
            dictionary.as_ref().and_then(|dictionary| {
                dictionary
                    .find(&CFString::new(key))
                    .and_then(|value| value.downcast::<CFBoolean>())
                    .map(bool::from)
            })
        };
        let front = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication();
        Presence {
            session_available: dictionary.is_some(),
            on_console: boolean("kCGSSessionOnConsoleKey") == Some(true),
            login_done: boolean("kCGSessionLoginDoneKey")
                .or_else(|| boolean("kCGSSessionLoginDoneKey"))
                == Some(true),
            // This key is added by WindowServer when locked. A malformed
            // present value is locked; an unavailable session is never safe.
            locked: dictionary.as_ref().is_none_or(|dictionary| {
                dictionary
                    .find(&CFString::new("CGSSessionScreenIsLocked"))
                    .is_some_and(|value| value.downcast::<CFBoolean>().is_none_or(bool::from))
            }),
            idle_seconds: unsafe { CGEventSourceSecondsSinceLastEventType(1, u32::MAX) },
            browser_frontmost: pid != 0
                && front.is_some_and(|application| application.processIdentifier() == pid as i32),
            input_sequence: u64::from(unsafe { CGEventSourceCounterForEventType(1, u32::MAX) }),
        }
    }
    fn process_birth(pid: u32) -> Option<(u64, u64)> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        if unsafe {
            libc::proc_pidinfo(
                pid as i32,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size as i32,
            )
        } != size as i32
        {
            return None;
        }
        let info = unsafe { info.assume_init() };
        (info.pbi_ppid == std::process::id() && info.pbi_uid == unsafe { libc::geteuid() })
            .then_some((info.pbi_start_tvsec, info.pbi_start_tvusec))
    }
    #[derive(Debug, PartialEq, Eq)]
    struct ChildIdentity {
        birth: (u64, u64),
        path: std::path::PathBuf,
        inode: u64,
    }
    fn child_identity(pid: u32) -> Option<ChildIdentity> {
        use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
        let birth = process_birth(pid)?;
        let mut raw = [0u8; 4096];
        if unsafe { libc::proc_pidpath(pid as i32, raw.as_mut_ptr().cast(), raw.len() as u32) } <= 0
        {
            return None;
        }
        let end = raw.iter().position(|byte| *byte == 0)?;
        let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&raw[..end]));
        let inode = std::fs::metadata(&path).ok()?.ino();
        Some(ChildIdentity { birth, path, inode })
    }
    async fn pair(
        home: &Path,
        pid: u32,
        expected: &ChildIdentity,
    ) -> Result<Zeroizing<Vec<u8>>, String> {
        let path = home
            .join("local_browser")
            .join(format!("companion-{pid}.sock"));
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut socket = loop {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(socket) => break socket,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(_) => return Err("native pairing unavailable".into()),
            }
        };
        let credential = socket.peer_cred().map_err(|_| "native peer unavailable")?;
        let mut peer_pid: libc::pid_t = 0;
        let mut size = std::mem::size_of_val(&peer_pid) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_LOCAL,
                libc::LOCAL_PEERPID,
                (&mut peer_pid as *mut libc::pid_t).cast(),
                &mut size,
            )
        } != 0
            || peer_pid != pid as i32
            || credential.uid() != unsafe { libc::geteuid() }
            || child_identity(pid).as_ref() != Some(expected)
        {
            return Err("native peer mismatch".into());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let byte = socket
                    .read_u8()
                    .await
                    .map_err(|_| "native pairing interrupted")?;
                if byte == b'\n' {
                    break;
                }
                if bytes.len() >= 64 {
                    return Err("native pairing oversized");
                }
                bytes.push(byte);
            }
            Ok::<(), &str>(())
        })
        .await
        .map_err(|_| "native pairing expired")??;
        let key = Zeroizing::new(
            base64::engine::general_purpose::STANDARD
                .decode(&*bytes)
                .map_err(|_| "native pairing invalid")?,
        );
        if key.len() != 32 {
            return Err("native pairing invalid".into());
        }
        if child_identity(pid).as_ref() != Some(expected) {
            return Err("native peer changed".into());
        }
        let account = wire::account(home);
        security_framework::passwords::set_generic_password(wire::SERVICE, &account, &key)
            .map_err(|_| "native keychain unavailable")?;
        let restored = Zeroizing::new(
            security_framework::passwords::get_generic_password(wire::SERVICE, &account)
                .map_err(|_| "native keychain unavailable")?,
        );
        if *restored != *key {
            return Err("native keychain verification failed".into());
        }
        Ok(restored)
    }
    async fn send(
        client: &reqwest::Client,
        port: u16,
        key: &[u8],
        body: Value,
    ) -> Result<Value, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "native clock unavailable")?
            .as_secs();
        let nonce: [u8; 16] = rand::random();
        let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        let body = serde_json::to_vec(&body).map_err(|_| "native request invalid")?;
        let proof = wire::sign(key, false, now, &nonce, &body)?;
        let mut response = client
            .post(format!("http://127.0.0.1:{port}{}", wire::ROUTE))
            .header("x-local-time", now)
            .header("x-local-nonce", &nonce)
            .header("x-local-signature", proof)
            .body(body)
            .send()
            .await
            .map_err(|_| "native gateway unavailable")?;
        if !response.status().is_success() {
            return Err("native gateway refused".into());
        }
        let signature = response
            .headers()
            .get("x-local-signature")
            .and_then(|value| value.to_str().ok())
            .ok_or("native gateway proof missing")?
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "native response interrupted")?
        {
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err("native response oversized".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let current = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "native clock unavailable")?
            .as_secs();
        if !wire::verify(key, true, now, &nonce, &bytes, &signature, current) {
            return Err("native gateway proof invalid".into());
        }
        serde_json::from_slice(&bytes).map_err(|_| "native response invalid".into())
    }

    fn native_client() -> Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| "native HTTP unavailable".into())
    }
    fn complete_review_details(card: &Value) -> Result<(&str, u64), &'static str> {
        let details = card["details"]
            .as_str()
            .filter(|details| !details.is_empty() && details.len() <= 64 * 1024)
            .ok_or("native action details unavailable")?;
        let seconds = card["read_consent_seconds"]
            .as_u64()
            .filter(|seconds| matches!(seconds, 0 | 60))
            .ok_or("native consent scope unavailable")?;
        Ok((details, seconds))
    }
    fn native_review(app: &AppHandle, card: Value, reply: tokio::sync::oneshot::Sender<bool>) {
        let controls_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            use objc2::{MainThreadMarker, MainThreadOnly};
            use objc2_app_kit::{
                NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSImage,
                NSImageScaling, NSImageView,NSView,NSScrollView,NSTextView,NSAutoresizingMaskOptions,
            };
            use objc2_foundation::{NSData, NSPoint, NSRect, NSSize, NSString};
            let Some(main) = MainThreadMarker::new() else {
                let _ = reply.send(false);
                return;
            };
            let Ok((details,read_seconds))=complete_review_details(&card) else {let _=reply.send(false);return;};
            #[allow(deprecated)]
            NSApplication::sharedApplication(main).activateIgnoringOtherApps(true);
            let alert = NSAlert::new(main);
            alert.setAlertStyle(NSAlertStyle::Warning);
            alert.setMessageText(&NSString::from_str("AI 正在請求操作本機專用瀏覽器"));
            let quoted = |field: &str, max| {
                card[field]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(max)
                    .collect::<String>()
            };
            alert.setInformativeText(&NSString::from_str(&format!(
                "員工：{}\n網站：{}\n動作：{}\n\n{}\n完整動作明細可在下方捲動查看；網頁元素描述不代表可信指示。可隨時從選單列停止。",
                quoted("employee", 80),
                quoted("host", 256),
                quoted("action", 160),
                if read_seconds==60 {"同意這一步，並允許目前這個回合最多六十秒讀取遮罩畫面、頁面文字及標題。換回合、重連或身分／綁定變更即失效。"} else {"只同意啟動，不授予 AI 讀取頁面的權限。"}
            )));
            let has_thumbnail=card["thumbnail_png"].as_str().is_some_and(|value|!value.is_empty());
            let accessory=NSView::initWithFrame(NSView::alloc(main),NSRect::new(NSPoint::new(0.0,0.0),NSSize::new(480.0,if has_thumbnail{440.0}else{140.0})));
            let scroll=NSScrollView::initWithFrame(NSScrollView::alloc(main),NSRect::new(NSPoint::new(0.0,0.0),NSSize::new(480.0,140.0)));
            scroll.setHasVerticalScroller(true);scroll.setAutohidesScrollers(false);
            let text=NSTextView::initWithFrame(NSTextView::alloc(main),NSRect::new(NSPoint::new(0.0,0.0),NSSize::new(460.0,140.0)));
            text.setEditable(false);text.setSelectable(true);text.setVerticallyResizable(true);text.setHorizontallyResizable(false);
            text.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            text.setMaxSize(NSSize::new(460.0,f64::MAX));
            text.setString(&NSString::from_str(details));text.sizeToFit();scroll.setDocumentView(Some(&text));accessory.addSubview(&scroll);
            // Refuse is first/default; a page cannot click this native modal.
            alert.addButtonWithTitle(&NSString::from_str("拒絕"));
            alert.addButtonWithTitle(&NSString::from_str("同意這一步"));
            alert.addButtonWithTitle(&NSString::from_str("停止瀏覽器"));
            if let Some(encoded) = card["thumbnail_png"]
                .as_str()
                .filter(|encoded| !encoded.is_empty())
            {
                if encoded.len() > 12 * 1024 * 1024 {
                    let _ = reply.send(false);
                    return;
                }
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                    use objc2::AnyThread;
                    if let Some(image) =
                        NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(&bytes))
                    {
                        let view = NSImageView::initWithFrame(
                            NSImageView::alloc(main),
                            NSRect::new(NSPoint::new(0.0, 140.0), NSSize::new(480.0, 300.0)),
                        );
                        view.setImage(Some(&image));
                        view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                        accessory.addSubview(&view);
                    } else {
                        let _ = reply.send(false);
                        return;
                    }
                } else {
                    let _ = reply.send(false);
                    return;
                }
            }
            alert.setAccessoryView(Some(&accessory));
            let result = alert.runModal();
            if result == NSAlertFirstButtonReturn + 2 {
                emergency_stop(&controls_app);
            }
            let _ = reply.send(result == NSAlertFirstButtonReturn + 1);
        });
    }
    pub(super) async fn run(
        app: AppHandle,
        manager: Arc<crate::sidecar::SidecarManager>,
        pid: u32,
        generation: u64,
        port: u16,
    ) -> Result<(), String> {
        let home = crate::lifecycle::duduclaw_home()
            .canonicalize()
            .map_err(|_| "native home unavailable")?;
        let identity = child_identity(pid).ok_or("native child unavailable")?;
        let key = pair(&home, pid, &identity).await?;
        let client = native_client()?;
        // A new paired sidecar cannot consume a modal intent queued for its
        // predecessor. This is independent of a dashboard/browser session.
        app.state::<NativeControls>().invalidate_start();
        app.state::<NativeControls>()
            .ready
            .store(false, Ordering::Release);
        let mut browser_pid = 0;
        let mut last_active = None;
        let mut last_start_result = None::<String>;
        let mut pending = None::<(String, String, tokio::sync::oneshot::Receiver<bool>)>;
        let mut last_card = None::<String>;
        loop {
            if !manager.local_child_is_current(pid, generation)
                || child_identity(pid).as_ref() != Some(&identity)
            {
                break;
            }
            let state = app.state::<NativeControls>();
            if state.stop.swap(false, Ordering::AcqRel) {
                let _ = send(&client, port, &key, json!({"op":"stop"})).await;
            }
            let report = presence(browser_pid);
            let answer = send(&client, port, &key, json!({"op":"poll","presence":report})).await;
            if let Ok(answer) = answer {
                if !manager.local_child_is_current(pid, generation)
                    || child_identity(pid).as_ref() != Some(&identity)
                {
                    break;
                }
                state.ready.store(
                    answer["ok"] == true && answer["hold"].is_null(),
                    Ordering::Release,
                );
                browser_pid = answer["active"]["browser_pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
                    .unwrap_or(0);
                let employee = answer["active"]["employee"].as_str().unwrap_or("");
                let label = if answer["active"].is_object() {
                    format!(
                        "● AI 正在使用瀏覽器（{employee}）{} ",
                        if answer["hold"].is_null() {
                            ""
                        } else {
                            "— 已凍結"
                        }
                    )
                } else {
                    "本機 AI 瀏覽器：待命".into()
                };
                *state.label.lock().unwrap_or_else(|p| p.into_inner()) = label.clone();
                if let Some(tray) = app.tray_by_id("main-tray") {
                    let _ = tray.set_tooltip(Some(&label));
                }
                let active = answer["active"].is_object();
                if last_active != Some(active) {
                    if let (Some(tray), Some(icon)) =
                        (app.tray_by_id("main-tray"), app.default_window_icon())
                    {
                        let _ = tray.set_icon(Some(if active {
                            active_icon(icon)
                        } else {
                            icon.clone()
                        }));
                    }
                    last_active = Some(active);
                }
                if let Some(tray) = app.tray_by_id("main-tray") {
                    let _ = tray.set_title(Some(if answer["active"].is_object() {
                        "AI 瀏覽器使用中"
                    } else {
                        ""
                    }));
                }
                if state.resume.swap(false, Ordering::AcqRel) {
                    let _ = send(&client, port, &key, json!({"op":"continue"})).await;
                }
                if let Some(id) = answer["approval"]["id"].as_str() {
                    if last_card.as_deref() != Some(id) && pending.is_none() {
                        if let Some(fingerprint) = answer["approval"]["fingerprint"].as_str() {
                            let (tx, rx) = tokio::sync::oneshot::channel();
                            pending = Some((id.into(), fingerprint.into(), rx));
                            last_card = Some(id.into());
                            native_review(&app, answer["approval"].clone(), tx);
                        }
                    }
                }
                if let Some((id, fingerprint, receiver)) = &mut pending {
                    match receiver.try_recv() {
                        Ok(approve) => {
                            let _=send(&client,port,&key,json!({"op":"decision","id":id,"fingerprint":fingerprint,"approve":approve})).await;
                            pending = None;
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => pending = None,
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
                    }
                }
                let request = state.take_start_request();
                if let Some(employee) = request {
                    let accepted = send(
                        &client,
                        port,
                        &key,
                        json!({"op":"start","employee":employee}),
                    )
                    .await;
                    if !accepted.is_ok_and(|answer| answer["ok"] == true) {
                        notice(&app,"未確認是否啟動，已避免自動重試。請查看本機指示；需要時先按停止，再由新的原生操作重試。");
                    }
                }
                if let Some(id) = answer["start_result"]["id"].as_str() {
                    if last_start_result.as_deref() != Some(id) {
                        last_start_result = Some(id.into());
                        if answer["start_result"]["ok"] != true {
                            notice(&app,"瀏覽器未能啟動：本機通道、設定或負責人綁定可能已變更。請確認後重新發起原生操作。");
                        }
                    }
                }
            } else {
                state.ready.store(false, Ordering::Release);
                *state.label.lock().unwrap_or_else(|p| p.into_inner()) =
                    "本機 AI 瀏覽器：已失聯，操作凍結".into();
            }
            tokio::time::sleep(Duration::from_millis(750)).await;
        }
        app.state::<NativeControls>()
            .ready
            .store(false, Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use tokio::io::AsyncWriteExt;
        #[test]
        fn native_review_requires_complete_details_and_a_known_consent_scope() {
            let details = format!("{}VISIBLE_TAIL", "full-action\n".repeat(1000));
            let card = json!({"details":details,"read_consent_seconds":60});
            assert!(complete_review_details(&card)
                .unwrap()
                .0
                .ends_with("VISIBLE_TAIL"));
            assert!(complete_review_details(&json!({"details":"kind only"})).is_err());
            assert!(
                complete_review_details(&json!({"details":"full","read_consent_seconds":600}))
                    .is_err()
            );
            assert!(complete_review_details(
                &json!({"details":"x".repeat(65537),"read_consent_seconds":0})
            )
            .is_err());
        }
        #[test]
        fn native_start_intents_expire_are_single_use_and_do_not_survive_stop_or_generation_change()
        {
            let controls = NativeControls::default();
            controls.ready.store(true, Ordering::Release);
            let intent = |generation, expired| NativeStartIntent {
                employee: "alice".into(),
                generation,
                expires: if expired {
                    Instant::now()
                } else {
                    Instant::now() + Duration::from_secs(60)
                },
            };
            *controls.start_request.lock().unwrap() = Some(intent(0, false));
            assert_eq!(controls.take_start_request().as_deref(), Some("alice"));
            assert!(controls.take_start_request().is_none());
            *controls.start_request.lock().unwrap() = Some(intent(0, true));
            assert!(controls.take_start_request().is_none());
            *controls.start_request.lock().unwrap() = Some(intent(0, false));
            controls.invalidate_start();
            assert!(controls.take_start_request().is_none());
            controls.stop.store(false, Ordering::Release);
            *controls.start_request.lock().unwrap() = Some(intent(0, false));
            assert!(controls.take_start_request().is_none());
            *controls.start_request.lock().unwrap() = Some(intent(1, false));
            controls.ready.store(false, Ordering::Release);
            assert!(controls.take_start_request().is_none());
        }
        #[test]
        fn active_indicator_changes_real_bitmap_colors_and_preserves_transparency() {
            let source = tauri::image::Image::new_owned(vec![1, 2, 3, 255, 40, 50, 60, 0], 2, 1);
            let active = active_icon(&source);
            assert_ne!(&active.rgba()[..3], &source.rgba()[..3]);
            assert_eq!(&active.rgba()[4..], &source.rgba()[4..]);
            assert_eq!((active.width(), active.height()), (2, 1));
        }

        #[tokio::test]
        async fn actual_native_http_client_requires_the_response_signature_bound_to_its_nonce() {
            for authentic in [true, false] {
                let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                    .await
                    .unwrap();
                let port = listener.local_addr().unwrap().port();
                let key = [0x17u8; 32];
                let fixture = tokio::spawn(async move {
                    let (mut peer, _) = listener.accept().await.unwrap();
                    let mut header = Vec::new();
                    while !header.ends_with(b"\r\n\r\n") {
                        assert!(header.len() < 8192);
                        header.push(peer.read_u8().await.unwrap());
                    }
                    let header = String::from_utf8(header).unwrap();
                    let field = |name: &str| {
                        header
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find(|(key, _)| key.eq_ignore_ascii_case(name))
                            .map(|(_, value)| value.trim().to_owned())
                            .unwrap()
                    };
                    let now: u64 = field("x-local-time").parse().unwrap();
                    let nonce = field("x-local-nonce");
                    let length: usize = field("content-length").parse().unwrap();
                    assert!(length < 8192);
                    let mut body = vec![0; length];
                    peer.read_exact(&mut body).await.unwrap();
                    assert!(wire::verify(
                        &key,
                        false,
                        now,
                        &nonce,
                        &body,
                        &field("x-local-signature"),
                        now
                    ));
                    let response = br#"{"ok":true}"#;
                    let mut other_nonce = nonce.clone();
                    other_nonce.replace_range(..1, if nonce.starts_with('0') { "1" } else { "0" });
                    let proof = wire::sign(
                        &key,
                        true,
                        now,
                        if authentic { &nonce } else { &other_nonce },
                        response,
                    )
                    .unwrap();
                    peer.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nx-local-signature: {proof}\r\nConnection: close\r\n\r\n",response.len()).as_bytes()).await.unwrap();
                    peer.write_all(response).await.unwrap();
                });
                let answer = send(
                    &native_client().unwrap(),
                    port,
                    &key,
                    json!({"op":"poll","fixture":true}),
                )
                .await;
                assert_eq!(answer.is_ok(), authentic);
                fixture.await.unwrap();
            }
        }

        #[test]
        fn the_app_itself_is_never_accepted_as_its_owned_gateway_child() {
            assert!(process_birth(std::process::id()).is_none());
        }
    }
}
