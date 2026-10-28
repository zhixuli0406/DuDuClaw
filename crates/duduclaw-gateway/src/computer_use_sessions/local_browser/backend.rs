//! Dedicated headed browser, driven only over inherited CDP descriptors.
//! Every page read and action requires the current runtime's host proof.
use super::super::{SessionBackend, SessionTarget, attended_call as invocation};
use super::{
    cdp_pipe::CdpPipe,
    companion::{ApprovalCard, Companion},
    launch, page_guard,
    policy::LocalBrowserConfig,
    profile::LocalProfile,
    proxy::ConnectProxy,
};
use crate::computer_use::{ComputerAction, ComputerUseError, mask_screenshot_regions};
use crate::computer_use_orchestrator::{
    ComputerUseConfig, FullMaskReason, MaskedScreenshot, NavigateOutcome, OrchestratorControl,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn error(why: impl Into<String>) -> ComputerUseError {
    ComputerUseError::Unavailable(why.into())
}

struct Runtime {
    pipe: Arc<CdpPipe>,
    page: String,
    profile: Arc<LocalProfile>,
    stop: CancellationToken,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub(crate) struct LocalBrowserBackend {
    home: PathBuf,
    agent: String,
    config: ComputerUseConfig,
    companion: Arc<Companion>,
    control: Arc<OrchestratorControl>,
    normal_exit: Arc<AtomicBool>,
    id: Option<String>,
    runtime: Option<Runtime>,
    read_consent: Arc<super::consent::ReadConsent>,
    native_start: Option<super::native_start::Authorization>,
}
impl LocalBrowserBackend {
    /// Literal expressions in the independently reviewed native fixture only;
    /// there is no wire/env entry for arbitrary CDP evaluation in a product.
    #[cfg(all(test, target_os = "macos"))]
    pub(super) async fn fixture_eval(&self, expression: &str) -> Result<Value, String> {
        let runtime = self.gate()?;
        let result=runtime.pipe.call("Runtime.evaluate",json!({"expression":expression,"returnByValue":true,"awaitPromise":true,"userGesture":true}),Some(&runtime.page)).await?;
        if result.get("exceptionDetails").is_some() {
            return Err("synthetic evaluation failed".into());
        }
        Ok(result["result"]["value"].clone())
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) async fn fixture_download_policy(&self, allow: bool) -> Result<(), String> {
        let runtime = self.gate()?;
        super::native_fixture::allow_download_control(allow);
        runtime.pipe.call("Browser.setDownloadBehavior",json!({"behavior":if allow{"allow"}else{"deny"},"eventsEnabled":true,"downloadPath":runtime.profile.path.parent().unwrap().join("downloads-disabled")}),None).await?;
        Ok(())
    }
    #[cfg(all(test, target_os = "macos"))]
    pub(super) async fn fixture_raw_capture_for_pixel_control(&self) -> Result<String, String> {
        if !super::native_fixture::active() {
            return Err("owned native fixture required".into());
        }
        let runtime = self.gate()?;
        page_guard::frame(&runtime.pipe, &runtime.page, &self.hosts()).await?;
        let capture = runtime.pipe.call("Page.captureScreenshot", json!({"format":"png","fromSurface":true,"captureBeyondViewport":false,
            "clip":{"x":0,"y":0,"width":self.config.display_width,"height":self.config.display_height,"scale":1}}), Some(&runtime.page)).await?;
        self.gate()?;
        capture["data"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "synthetic pixel control unavailable".into())
    }
    fn start_gate(&self) -> Result<(), ComputerUseError> {
        let caps = super::super::agent_capabilities(&self.home, &self.agent);
        if !(invocation::current(&self.agent)
            || self.native_start.as_ref().is_some_and(|authorization| {
                authorization
                    .check(&self.home, &self.agent, &self.companion)
                    .is_ok()
            }))
            || self.control.stopped.load(Ordering::Acquire)
            || self.control.paused.load(Ordering::Acquire)
            || !self.companion.ready()
            || !LocalBrowserConfig::load(&self.home).is_ok_and(|config| config.enabled)
            || !caps.computer_use
            || !caps.computer_use_config.local_browser
            || crate::computer_use_orchestrator::read_threat_level_sync(&self.home)
                != crate::computer_use_orchestrator::ThreatLevel::Green
        {
            return Err(error("local_companion_or_live_runtime_required"));
        }
        Ok(())
    }
    pub(crate) fn new(
        home: PathBuf,
        agent: String,
        config: ComputerUseConfig,
        companion: Arc<Companion>,
    ) -> Self {
        Self {
            home,
            agent,
            config,
            companion,
            control: Arc::new(OrchestratorControl::new()),
            normal_exit: Arc::new(AtomicBool::new(false)),
            id: None,
            runtime: None,
            read_consent: Arc::new(super::consent::ReadConsent::default()),
            native_start: None,
        }
    }
    pub(crate) fn with_native_start(
        mut self,
        authorization: Option<super::native_start::Authorization>,
    ) -> Self {
        self.native_start = authorization;
        self
    }
    fn read_grant_valid(&self) -> Result<(), String> {
        let call = invocation::current_call(&self.agent).ok_or("local_live_runtime_required")?;
        let binding = super::consent::binding(&self.home, &self.agent)?;
        if !self.read_consent.allows(&call, &binding) {
            return Err("local_read_consent_required".into());
        }
        self.gate().map(|_| ())
    }
    async fn ensure_read_consent(&self) -> Result<(), String> {
        if self.read_grant_valid().is_err() {
            self.review(json!({"kind":"read_current_live_turn","expires_seconds":60}))
                .await?;
        }
        self.read_grant_valid()
    }
    fn hosts(&self) -> Vec<String> {
        self.config
            .pinned_hosts
            .iter()
            .map(|host| host.host.clone())
            .collect()
    }
    fn gate(&self) -> Result<&Runtime, String> {
        if !invocation::current(&self.agent) {
            return Err("local_live_runtime_required".into());
        }
        if self.control.stopped.load(Ordering::Acquire) {
            return Err("local_session_ended".into());
        }
        if self.control.paused.load(Ordering::Acquire) {
            return Err("local_session_paused".into());
        }
        if crate::computer_use_orchestrator::read_threat_level_sync(&self.home)
            != crate::computer_use_orchestrator::ThreatLevel::Green
        {
            return Err("local_threat_not_green".into());
        }
        if !LocalBrowserConfig::load(&self.home)?.enabled
            || !super::super::agent_capabilities(&self.home, &self.agent)
                .computer_use_config
                .local_browser
        {
            return Err("local_browser_disabled".into());
        }
        self.companion
            .gate(self.id.as_deref().ok_or("local_session_ended")?)?;
        let runtime = self.runtime.as_ref().ok_or("local_session_ended")?;
        if runtime.stop.is_cancelled() || !runtime.profile.valid() {
            return Err("local_owner_changed".into());
        }
        Ok(runtime)
    }
    async fn observation(&self, expression: &str) -> Result<Value, String> {
        let runtime = self.gate()?;
        let answer =
            page_guard::evaluate(&runtime.pipe, &runtime.page, &self.hosts(), expression).await?;
        self.gate()?;
        Ok(answer)
    }
    async fn shot(&self) -> Result<MaskedScreenshot, ComputerUseError> {
        let runtime = self.gate().map_err(error)?;
        let hosts = self.hosts();
        page_guard::frame(&runtime.pipe, &runtime.page, &hosts)
            .await
            .map_err(error)?;
        self.gate().map_err(error)?;
        // The detector and raster capture currently occur in separate render
        // steps. The native CSS-race regression must close that gap before
        // this feature can ship. Raw pixels never leave this method.
        let title = self.observation("document.title").await;
        let regions = self.observation(page_guard::MASK_REGIONS).await;
        let full_mask = if title.as_ref().ok().and_then(Value::as_str).is_none() {
            Some(FullMaskReason::TitleUnreadable)
        } else if crate::computer_use_orchestrator::window_is_sensitive(
            title.as_ref().ok().and_then(Value::as_str),
        ) {
            Some(FullMaskReason::TitleSensitive)
        } else if regions.is_err() {
            Some(FullMaskReason::HelperFailed)
        } else {
            None
        };
        let rectangles: Result<Vec<[u32; 4]>, _> = regions.and_then(|value| {
            serde_json::from_value(value).map_err(|_| "mask data invalid".into())
        });
        let full_mask = full_mask.or_else(|| {
            rectangles
                .as_ref()
                .err()
                .map(|_| FullMaskReason::HelperFailed)
        });
        let rectangles = if full_mask.is_some() {
            vec![[0, 0, self.config.display_width, self.config.display_height]]
        } else {
            rectangles.map_err(error)?
        };
        #[cfg(all(test, target_os = "macos"))]
        super::native_fixture::before_mask_capture(&rectangles).await;
        let capture=runtime.pipe.call("Page.captureScreenshot",json!({"format":"png","fromSurface":true,"captureBeyondViewport":false,
            "clip":{"x":0,"y":0,"width":self.config.display_width,"height":self.config.display_height,"scale":1}}),Some(&runtime.page)).await.map_err(error)?;
        #[cfg(all(test, target_os = "macos"))]
        super::native_fixture::after_mask_capture().await;
        self.gate().map_err(error)?;
        page_guard::frame(&runtime.pipe, &runtime.page, &hosts)
            .await
            .map_err(error)?;
        let png = capture["data"]
            .as_str()
            .ok_or_else(|| error("viewport unavailable"))?;
        let png_base64 = mask_screenshot_regions(png, &rectangles, [0, 0, 0])?;
        Ok(MaskedScreenshot {
            png_base64,
            full_mask,
        })
    }
    async fn click_description(&self, action: &Value) -> Result<Option<String>, String> {
        if action["kind"] != "click" {
            return Ok(None);
        }
        let point = &action["action"]["coordinate"];
        let x = point[0].as_u64().ok_or("local_click_target_unavailable")?;
        let y = point[1].as_u64().ok_or("local_click_target_unavailable")?;
        let expression = format!(
            "(()=>{{let e=document.elementFromPoint({x},{y});if(!e)return null;e=e.closest('button,a,label,summary,[role=button]')||e;return String(e.getAttribute('aria-label')||(['INPUT','TEXTAREA'].includes(e.tagName)?e.tagName+' '+(e.getAttribute('type')||''):e.innerText||e.tagName)).slice(0,120)}})()"
        );
        self.observation(&expression)
            .await?
            .as_str()
            .map(|label| Some(label.to_owned()))
            .ok_or_else(|| "local_click_target_unavailable".into())
    }
    async fn review(&self, action: Value) -> Result<(), String> {
        let runtime = self.gate()?;
        let call = invocation::current_call(&self.agent).ok_or("local_live_runtime_required")?;
        let binding = super::consent::binding(&self.home, &self.agent)?;
        let policy = crate::approval::policy_revision(&self.home, &self.agent)?;
        let frame = runtime
            .pipe
            .call("Page.getFrameTree", json!({}), Some(&runtime.page))
            .await?;
        let url = frame["frameTree"]["frame"]["url"]
            .as_str()
            .ok_or("page unavailable")?;
        if !page_guard::allowed_url(url, &self.hosts(), true) {
            return Err("local_navigation_refused".into());
        }
        let host = url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "about:blank".into());
        let thumbnail = self
            .shot()
            .await
            .map_err(|_| "local_approval_preview_unavailable")?;
        let screenshot_hash = crate::approval::payload_hash(&json!(thumbnail.png_base64));
        let label = self.click_description(&action).await?;
        let details = super::review_details::render(&action, label.as_deref())?;
        let fingerprint = crate::approval::payload_hash(
            &json!({"session":self.id,"page":runtime.page,"policy":policy,"screenshot_hash":screenshot_hash,"url_hash":crate::approval::payload_hash(&json!(url)),"action":action,"native_details_hash":crate::approval::payload_hash(&json!(details))}),
        );
        self.companion
            .approve(ApprovalCard {
                id: uuid::Uuid::new_v4().to_string(),
                fingerprint,
                employee: self.agent.clone(),
                host,
                action: action["kind"].as_str().unwrap_or("action").into(),
                details,
                read_consent_seconds: 60,
                thumbnail_png: thumbnail.png_base64,
            })
            .await?;
        self.gate()?;
        if crate::approval::policy_revision(&self.home, &self.agent)? != policy {
            return Err("local_approval_stale".into());
        }
        let fresh = self
            .shot()
            .await
            .map_err(|_| "local_approval_preview_unavailable")?;
        if crate::approval::payload_hash(&json!(fresh.png_base64)) != screenshot_hash {
            return Err("local_approval_stale".into());
        }
        // A redirect while the native dialog was open invalidates its grant.
        let after = runtime
            .pipe
            .call("Page.getFrameTree", json!({}), Some(&runtime.page))
            .await?;
        if after["frameTree"]["frame"]["url"] != frame["frameTree"]["frame"]["url"] {
            return Err("local_approval_stale".into());
        }
        if self.click_description(&action).await? != label {
            return Err("local_approval_stale".into());
        }
        if !invocation::current_call(&self.agent)
            .is_some_and(|current| call.same_live_call(&current))
            || super::consent::binding(&self.home, &self.agent)? != binding
        {
            return Err("local_approval_stale".into());
        }
        self.read_consent.record(call, binding);
        Ok(())
    }
}

#[async_trait]
impl SessionBackend for LocalBrowserBackend {
    fn normal_local_exit(&self) -> bool {
        self.normal_exit.load(Ordering::Acquire)
    }
    fn local_read_consent(&self) -> Option<Arc<super::consent::ReadConsent>> {
        Some(self.read_consent.clone())
    }
    fn target(&self) -> SessionTarget {
        SessionTarget::LocalBrowser
    }
    async fn native_review(&self, action: Value) -> Result<(), String> {
        self.review(action).await
    }
    async fn register(&mut self, id: &str) -> Result<(), ComputerUseError> {
        self.companion
            .bind(id, &self.agent, &self.control)
            .map_err(error)?;
        if let Err(why) =
            crate::computer_use_orchestrator::register_session(id, self.control.clone()).await
        {
            self.companion.unbind(id);
            return Err(why);
        }
        self.id = Some(id.into());
        Ok(())
    }
    async fn start(&mut self) -> Result<(), ComputerUseError> {
        self.normal_exit.store(false, Ordering::Release);
        self.start_gate()?;
        let id = self
            .id
            .clone()
            .ok_or_else(|| error("local_session_unregistered"))?;
        let policy = crate::approval::policy_revision(&self.home, &self.agent).map_err(error)?;
        self.companion
            .approve(ApprovalCard {
                id: uuid::Uuid::new_v4().to_string(),
                fingerprint: crate::approval::payload_hash(
                    &json!({"start":id,"agent":self.agent,"hosts":self.hosts(),"policy":policy}),
                ),
                employee: self.agent.clone(),
                host: self.hosts().join(", "),
                action: "start dedicated browser".into(),
                details: super::review_details::render(
                    &json!({"kind":"start","employee":self.agent,"allowed_hosts":self.hosts()}),
                    None,
                )
                .map_err(error)?,
                read_consent_seconds: 0,
                thumbnail_png: String::new(),
            })
            .await
            .map_err(error)?;
        self.start_gate()?;
        if crate::approval::policy_revision(&self.home, &self.agent).map_err(error)? != policy {
            return Err(error("local_approval_stale"));
        }
        let profile =
            Arc::new(LocalProfile::prepare(&self.home, &self.agent, self.hosts()).map_err(error)?);
        let proxy = Arc::new(ConnectProxy::start(self.hosts()).await.map_err(error)?);
        self.start_gate()?;
        profile.begin_browser_launch().map_err(error)?;
        let (mut child, socket) = launch::spawn(
            &self.home,
            &self.agent,
            proxy.address,
            self.config.display_width,
            self.config.display_height,
        )
        .await
        .map_err(error)?;
        let pid = child.id().ok_or_else(|| error("local_browser_exited"))?;
        let mut processes = match super::processes::OwnedGroup::new(pid) {
            Ok(processes) => processes,
            Err(reason) => {
                self.control.discard_login.store(true, Ordering::Release);
                let _ = child.kill().await;
                let _ = child.wait().await;
                super::audit::record(
                    &self.home,
                    &self.agent,
                    "local_cleanup_uncertain",
                    json!({"phase":"initial_identity","pid":pid}),
                );
                return Err(error(reason));
            }
        };
        if let Err(reason) = processes.sample() {
            let clean = processes.finish().await;
            let _ = child.kill().await;
            let _ = child.wait().await;
            if clean {
                let _ = profile.acknowledge_browser_exit();
            }
            self.control.discard_login.store(true, Ordering::Release);
            super::audit::record(
                &self.home,
                &self.agent,
                "local_cleanup_uncertain",
                json!({"phase":"initial_inventory","pid":pid,"cleanup_verified":clean}),
            );
            return Err(error(reason));
        }
        #[cfg(all(test, target_os = "macos"))]
        if let Err(reason) = super::native_fixture::track(&processes) {
            if processes.finish().await {
                let _ = profile.acknowledge_browser_exit();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error(reason));
        }
        self.companion.set_pid(&id, pid);
        super::audit::record(
            &self.home,
            &self.agent,
            "local_start",
            json!({"session_id":id,"pid":pid,"owned_processes":processes.manifest()}),
        );
        let (pipe, mut events) = CdpPipe::new(socket);
        let stop = CancellationToken::new();
        let cancel = stop.clone();
        let observer = pipe.clone();
        let control = self.control.clone();
        let normal_exit = self.normal_exit.clone();
        let companion = self.companion.clone();
        let owner = profile.clone();
        let home = self.home.clone();
        let agent = self.agent.clone();
        let hosts = self.hosts();
        let selected = Arc::new(std::sync::Mutex::new(None::<String>));
        #[cfg(all(test, target_os = "macos"))]
        let fixture = super::native_fixture::current();
        let selected_worker = selected.clone();
        let worker = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    _=cancel.cancelled()=>break,
                    _=tick.tick()=>{
                        if processes.sample().is_err() {control.discard_login.store(true,Ordering::Release);break;}
                        #[cfg(all(test,target_os="macos"))]
                        if super::native_fixture::refresh(fixture.as_deref(),&processes).is_err() {control.discard_login.store(true,Ordering::Release);break;}
                        let caps=super::super::agent_capabilities(&home,&agent);
                        if control.stopped.load(Ordering::Acquire) || companion.heartbeat_lost() || !owner.valid()
                            || !LocalBrowserConfig::load(&home).is_ok_and(|config|config.enabled)
                            || !caps.computer_use || !caps.computer_use_config.local_browser {break;}
                        match child.try_wait() {
                            Ok(None)=>{},
                            Ok(Some(status))=>{if status.success() && !control.stopped.load(Ordering::Acquire) && !control.discard_login.load(Ordering::Acquire) {normal_exit.store(true,Ordering::Release);}break;},
                            Err(_)=>{control.discard_login.store(true,Ordering::Release);break;}
                        }
                    },
                    event=events.recv()=>{
                        let Some(event)=event else {
                            if tokio::time::timeout(Duration::from_millis(250),child.wait()).await.is_ok_and(|status|status.is_ok_and(|status|status.success()))
                                && !control.stopped.load(Ordering::Acquire) && !control.discard_login.load(Ordering::Acquire) && owner.valid() {
                                normal_exit.store(true,Ordering::Release);
                            }
                            break;
                        };
                        if event["method"]=="Fetch.requestPaused" {
                            let raw=event["params"]["request"]["url"].as_str().unwrap_or("");
                            if !page_guard::allowed_url(raw,&hosts,false) {
                                let host=url::Url::parse(raw).ok().and_then(|url|url.host_str().map(str::to_owned));
                                super::audit::record(&home,&agent,"local_blocked_request",json!({"host":host}));
                            }
                        }
                        let page=selected_worker.lock().unwrap_or_else(|p|p.into_inner()).clone();
                        #[cfg(all(test,target_os="macos"))]
                        match super::native_fixture::fulfill(&observer,&event,fixture.as_deref()).await {
                            Ok(true)=>continue,
                            Ok(false)=>{},
                            Err(_)=>{control.discard_login.store(true,Ordering::Release);break;}
                        }
                        if let Err(reason)=security_event(&observer,&proxy,&hosts,page.as_deref(),event).await {
                            if reason=="local_window_closed" {normal_exit.store(true,Ordering::Release);}else{control.discard_login.store(true,Ordering::Release);}
                            break;
                        }
                    }
                }
            }
            control.stopped.store(true, Ordering::Release);
            cancel.cancel();
            let inventoried = processes.sample().is_ok();
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                observer.call("Browser.close", json!({}), None),
            )
            .await;
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            let cleanup_inventory = processes.manifest();
            let clean = processes.finish().await && inventoried;
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill().await;
            }
            let _ = child.wait().await;
            let acknowledged = clean && owner.acknowledge_browser_exit().is_ok();
            if acknowledged && !control.discard_login.load(Ordering::Acquire) {
                let _ = owner.mark_clean();
            } else {
                control.discard_login.store(true, Ordering::Release);
            }
            companion.unbind(&id);
            crate::computer_use_orchestrator::unregister_session(&id).await;
            super::audit::record(
                &home,
                &agent,
                "local_stop",
                json!({"by":"backend_exit","cleanup_verified":acknowledged,"owned_processes":cleanup_inventory}),
            );
        });
        // Install the owned cleanup task before the first fallible command.
        self.runtime = Some(Runtime {
            pipe: pipe.clone(),
            page: String::new(),
            profile,
            stop,
            worker,
        });
        let version = pipe
            .call("Browser.getVersion", json!({}), None)
            .await
            .map_err(error)?;
        if !version["product"]
            .as_str()
            .is_some_and(launch::version_supported)
        {
            return Err(error("local_browser_update_required"));
        }
        pipe.call(
            "Browser.setDownloadBehavior",
            json!({"behavior":"deny","eventsEnabled":true,"downloadPath":self.runtime.as_ref().unwrap().profile.path.parent().unwrap().join("downloads-disabled")}),
            None,
        )
        .await
        .map_err(error)?;
        pipe.call("Browser.grantPermissions", json!({"permissions":[]}), None)
            .await
            .map_err(error)?;
        let targets = pipe
            .call("Target.getTargets", json!({}), None)
            .await
            .map_err(error)?;
        let pages: Vec<_> = targets["targetInfos"]
            .as_array()
            .ok_or_else(|| error("browser targets unavailable"))?
            .iter()
            .filter(|target| target["type"] == "page")
            .collect();
        if pages.len() != 1 || pages[0]["url"] != "about:blank" {
            return Err(error("unexpected initial browser target"));
        }
        let target = pages[0]["targetId"]
            .as_str()
            .ok_or_else(|| error("browser target unavailable"))?
            .to_owned();
        *selected.lock().unwrap_or_else(|p| p.into_inner()) = Some(target.clone());
        pipe.call(
            "Target.setAutoAttach",
            json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true}),
            None,
        )
        .await
        .map_err(error)?;
        let attached = pipe
            .call(
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
                None,
            )
            .await
            .map_err(error)?;
        let page = attached["sessionId"]
            .as_str()
            .ok_or_else(|| error("browser page unavailable"))?
            .to_owned();
        self.runtime.as_mut().unwrap().page = page.clone();
        page_guard::install(
            &pipe,
            &page,
            self.config.display_width,
            self.config.display_height,
        )
        .await
        .map_err(error)?;
        pipe.call("Target.setDiscoverTargets", json!({"discover":true}), None)
            .await
            .map_err(error)?;
        pipe.call("Page.bringToFront", json!({}), Some(&page))
            .await
            .map_err(error)?;
        self.start_gate()?;
        if crate::approval::policy_revision(&self.home, &self.agent).map_err(error)? != policy {
            return Err(error("local_approval_stale"));
        }
        // A native standalone start never mints or seeds chat page-read consent.
        self.native_start = None;
        Ok(())
    }
    async fn stop(&mut self) {
        self.control.stopped.store(true, Ordering::Release);
        if let Some(mut runtime) = self.runtime.take() {
            runtime.stop.cancel();
            let _ = (&mut runtime.worker).await;
        }
        if let Some(id) = self.id.take() {
            self.companion.unbind(&id);
            crate::computer_use_orchestrator::unregister_session(&id).await;
        }
        if self.control.discard_login.load(Ordering::Acquire) {
            super::profile::forget_profile(&self.home, &self.agent)
                .map_err(|_| ())
                .ok();
        }
    }
    fn discard_login(&mut self) {
        self.control.discard_login.store(true, Ordering::Release);
    }
    async fn screenshot(&self) -> Result<MaskedScreenshot, ComputerUseError> {
        self.ensure_read_consent().await.map_err(error)?;
        let screenshot = self.shot().await?;
        self.read_grant_valid().map_err(error)?;
        Ok(screenshot)
    }
    async fn window_title(&self) -> Result<String, String> {
        self.ensure_read_consent().await?;
        let title = self
            .observation("document.title")
            .await?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "title unavailable".to_string())?;
        self.read_grant_valid()?;
        Ok(title)
    }
    async fn page_text(&self) -> Result<String, ComputerUseError> {
        self.ensure_read_consent().await.map_err(error)?;
        let text = self
            .observation("(document.body?.innerText||'').slice(0,65536)")
            .await
            .map_err(error)?
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| error("page text unavailable"))?;
        self.read_grant_valid().map_err(error)?;
        Ok(text)
    }
    async fn freeze(&self) -> Result<(), ComputerUseError> {
        self.control.paused.store(true, Ordering::Release);
        Ok(())
    }
    async fn thaw(&self) -> Result<(), ComputerUseError> {
        self.companion
            .gate(
                self.id
                    .as_deref()
                    .ok_or_else(|| error("local_session_ended"))?,
            )
            .map_err(error)?;
        self.control.paused.store(false, Ordering::Release);
        Ok(())
    }
    async fn navigate(&self, url: &str) -> Result<NavigateOutcome, ComputerUseError> {
        if !page_guard::allowed_url(url, &self.hosts(), false) {
            return Err(error("local_navigation_refused"));
        }
        self.review(json!({"kind":"navigate","url":url}))
            .await
            .map_err(error)?;
        let runtime = self.gate().map_err(error)?;
        let answer = runtime
            .pipe
            .call_checked(
                "Page.navigate",
                json!({"url":url}),
                Some(&runtime.page),
                || self.gate().map(|_| ()),
            )
            .await
            .map_err(error)?;
        Ok(NavigateOutcome {
            ok: answer.get("errorText").is_none(),
            host: url::Url::parse(url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned)),
            error: answer["errorText"].as_str().map(str::to_owned),
        })
    }
    async fn execute(&self, action: &ComputerAction) -> Result<(), ComputerUseError> {
        self.gate().map_err(error)?;
        if !matches!(action, ComputerAction::Scroll { .. }) {
            self.review(json!({"kind":match action {ComputerAction::Type{..}=>"type",ComputerAction::Key{..}=>"key",_=>"click"},"action":action})).await.map_err(error)?;
        }
        let runtime = self.gate().map_err(error)?;
        page_guard::frame(&runtime.pipe, &runtime.page, &self.hosts())
            .await
            .map_err(error)?;
        match action {
            ComputerAction::LeftClick { coordinate }
            | ComputerAction::RightClick { coordinate }
            | ComputerAction::DoubleClick { coordinate } => {
                // Context menus expose browser/OS operations outside the page.
                if matches!(action, ComputerAction::RightClick { .. }) {
                    return Err(error("local_context_menu_refused"));
                }
                let count = if matches!(action, ComputerAction::DoubleClick { .. }) {
                    2
                } else {
                    1
                };
                for kind in ["mousePressed", "mouseReleased"] {
                    self.gate().map_err(error)?;
                    runtime.pipe.call_checked("Input.dispatchMouseEvent",json!({"type":kind,"x":coordinate[0],"y":coordinate[1],"button":"left","clickCount":count}),Some(&runtime.page),||self.gate().map(|_|())).await.map_err(error)?;
                }
            }
            ComputerAction::Type { text } => {
                if self
                    .observation(page_guard::FOCUS_SAFE)
                    .await
                    .map_err(error)?
                    != true
                {
                    return Err(error("local_sensitive_input_refused"));
                }
                runtime
                    .pipe
                    .call_checked(
                        "Input.insertText",
                        json!({"text":text}),
                        Some(&runtime.page),
                        || self.gate().map(|_| ()),
                    )
                    .await
                    .map_err(error)?;
            }
            ComputerAction::Key { text } => {
                let (key, code, virtual_key) =
                    page_guard::key(text).ok_or_else(|| error("local_key_refused"))?;
                if self
                    .observation(page_guard::FOCUS_SAFE)
                    .await
                    .map_err(error)?
                    != true
                {
                    return Err(error("local_sensitive_input_refused"));
                }
                for kind in ["keyDown", "keyUp"] {
                    self.gate().map_err(error)?;
                    runtime.pipe.call_checked("Input.dispatchKeyEvent",json!({"type":kind,"key":key,"code":code,"windowsVirtualKeyCode":virtual_key}),Some(&runtime.page),||self.gate().map(|_|())).await.map_err(error)?;
                }
            }
            ComputerAction::Scroll {
                coordinate,
                direction,
                amount,
            } => {
                let delta = f64::from((*amount).min(10000)) * 100.0;
                let (x, y) = match direction.as_str() {
                    "up" => (0.0, -delta),
                    "down" => (0.0, delta),
                    "left" => (-delta, 0.0),
                    "right" => (delta, 0.0),
                    _ => return Err(error("local_scroll_refused")),
                };
                runtime.pipe.call_checked("Input.dispatchMouseEvent",json!({"type":"mouseWheel","x":coordinate[0],"y":coordinate[1],"deltaX":x,"deltaY":y}),Some(&runtime.page),||self.gate().map(|_|())).await.map_err(error)?;
            }
            _ => return Err(error("local_action_unsupported")),
        }
        self.gate().map_err(error)?;
        Ok(())
    }
    fn control(&self) -> Arc<OrchestratorControl> {
        self.control.clone()
    }
    fn login_state(&self) -> &str {
        "local_dedicated"
    }
    fn login_owner_valid(&self) -> bool {
        self.runtime
            .as_ref()
            .is_none_or(|runtime| runtime.profile.valid())
    }
}
impl Drop for LocalBrowserBackend {
    fn drop(&mut self) {
        self.control.stopped.store(true, Ordering::Release);
    }
}

async fn security_event(
    pipe: &CdpPipe,
    proxy: &ConnectProxy,
    hosts: &[String],
    selected: Option<&str>,
    event: Value,
) -> Result<(), String> {
    let params = &event["params"];
    let session = event["sessionId"].as_str();
    match event["method"].as_str().unwrap_or("") {
        "Target.attachedToTarget" => {
            if params["targetInfo"]["targetId"].as_str() != selected {
                pipe.call(
                    "Target.closeTarget",
                    json!({"targetId":params["targetInfo"]["targetId"]}),
                    None,
                )
                .await?;
            } else if let Some(page) = params["sessionId"].as_str() {
                pipe.call("Runtime.runIfWaitingForDebugger", json!({}), Some(page))
                    .await?;
            }
        }
        "Fetch.authRequired" => {
            let response = proxy.challenge_response(
                params["authChallenge"]["origin"].as_str().unwrap_or(""),
                params["authChallenge"]["source"].as_str().unwrap_or(""),
            );
            let credentials = match response {
                Some((username, password)) => {
                    json!({"response":"ProvideCredentials","username":username,"password":password})
                }
                None => json!({"response":"CancelAuth"}),
            };
            pipe.call(
                "Fetch.continueWithAuth",
                json!({"requestId":params["requestId"],"authChallengeResponse":credentials}),
                session,
            )
            .await?;
        }
        "Fetch.requestPaused" => {
            let allowed = page_guard::allowed_url(
                params["request"]["url"].as_str().unwrap_or(""),
                hosts,
                false,
            );
            pipe.call(
                if allowed {
                    "Fetch.continueRequest"
                } else {
                    "Fetch.failRequest"
                },
                if allowed {
                    json!({"requestId":params["requestId"]})
                } else {
                    json!({"requestId":params["requestId"],"errorReason":"BlockedByClient"})
                },
                session,
            )
            .await?;
        }
        "Target.targetCreated" => {
            if let Some(id) = params["targetInfo"]["targetId"].as_str() {
                if Some(id) != selected && params["targetInfo"]["type"] != "browser" {
                    pipe.call("Target.closeTarget", json!({"targetId":id}), None)
                        .await?;
                }
            }
        }
        "Target.targetDestroyed" if params["targetId"].as_str() == selected => {
            return Err("local_window_closed".into());
        }
        "Page.frameNavigated" => {
            if !page_guard::allowed_url(params["frame"]["url"].as_str().unwrap_or(""), hosts, true)
            {
                return Err("local_navigation_refused".into());
            }
        }
        "Page.fileChooserOpened" => {
            // No selected file is supplied. Ending the browser also dismisses
            // the intercepted chooser without showing an OS upload picker.
            return Err("local_upload_refused".into());
        }
        "Browser.downloadWillBegin" => {
            if let Some(guid) = params["guid"].as_str() {
                pipe.call("Browser.cancelDownload", json!({"guid":guid}), None)
                    .await?;
            }
        }
        "Page.javascriptDialogOpening" => {
            pipe.call(
                "Page.handleJavaScriptDialog",
                json!({"accept":false}),
                session,
            )
            .await?;
        }
        "Inspector.detached" => return Err("local_browser_detached".into()),
        _ => {}
    }
    Ok(())
}
