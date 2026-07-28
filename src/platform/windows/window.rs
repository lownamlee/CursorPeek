use std::{
    cell::Cell,
    error::Error as StdError,
    fmt,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use super::explorer::{
    belongs_to_explorer_window_at, is_explorer_window_at, is_foreground_explorer_window_at,
};
#[cfg(test)]
use super::input::registered_raw_devices;
use super::input::{
    RawInputActivity, RawInputRegistration, physical_cursor_position, read_raw_input_activity,
    system_hover_rectangle,
};

use crate::hover::{
    DEFAULT_DWELL_DELAY, DwellTimerEvent, Generation, HoverState, INPUT_SAMPLE_INTERVAL,
    InputCoverage, InputCoverageReport, PhysicalScreenPoint,
};
use crate::preview::{PreviewPlacement, PreviewSize};
use crate::settings::{SettingsDocument, SettingsFile, Theme};
use crate::worker::{
    CompletionNotifier, PendingWorkerPoll, PendingWorkerResolution, PreviewResult, WorkerManager,
    WorkerManagerError,
};
use cursorpeek_core::PhysicalScreenRect;

use super::{PreviewWindow, StartupRegistration, TrayCommand, TrayIcon, TrayMenuState, TrayStatus};

mod performance;
mod recovery;

use performance::ActivePerformanceDiagnostics;

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
                GetForegroundWindow, GetMessageW, KillTimer, MSG, PBT_APMRESUMEAUTOMATIC,
                PBT_APMRESUMECRITICAL, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PostMessageW,
                RegisterClassW, RegisterWindowMessageW, SPI_SETWORKAREA, SetTimer,
                TranslateMessage, USER_TIMER_MAXIMUM, USER_TIMER_MINIMUM, UnregisterClassW,
                WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_DISPLAYCHANGE, WM_INPUT,
                WM_POWERBROADCAST, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    },
    core::{Error, PCWSTR, Result, w},
};

#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

pub(super) const CLASS_NAME: PCWSTR = w!("CursorPeek.MessageWindow");
pub(super) const SHUTDOWN_MESSAGE: u32 = WM_APP + 1;
const WORKER_RESULT_MESSAGE: u32 = WM_APP + 2;
const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 3;
pub(super) const ACTIVATE_MESSAGE: u32 = WM_APP + 4;
const PREVIEW_CONTEXT_INVALIDATED_MESSAGE: u32 = WM_APP + 6;
const SYSTEM_LIFECYCLE_CHANGED_MESSAGE: u32 = WM_APP + 7;
const TRAY_EVENT_MESSAGE: u32 = WM_APP + 8;
const DWELL_TIMER_ID: usize = 1;
const INPUT_SAMPLE_TIMER_ID: usize = 2;
const INPUT_DIAGNOSTIC_DEADLINE_TIMER_ID: usize = 3;
const PREVIEW_DIAGNOSTIC_DEADLINE_TIMER_ID: usize = 4;
const PREVIEW_GUARD_TIMER_ID: usize = 5;
const PERFORMANCE_DIAGNOSTIC_DEADLINE_TIMER_ID: usize = 6;
const PREVIEW_GUARD_INTERVAL: Duration = Duration::from_millis(125);
static TASKBAR_CREATED_MESSAGE: AtomicU32 = AtomicU32::new(0);

pub(crate) const PREVIEW_WINDOW_DIAGNOSTIC_DURATION: Duration = Duration::from_millis(1_500);
pub(crate) const PREVIEW_WINDOW_PRACTICE_DURATION: Duration = Duration::from_secs(5);

#[cfg(test)]
const TEST_PANIC_MESSAGE: u32 = WM_APP + 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
enum SystemLifecycleChange {
    DisplayConfiguration = 1,
    WorkArea = 2,
    TaskbarCreated = 3,
    Suspend = 4,
    Resume = 5,
}

impl SystemLifecycleChange {
    const fn from_message_parameter(value: usize) -> Option<Self> {
        match value {
            1 => Some(Self::DisplayConfiguration),
            2 => Some(Self::WorkArea),
            3 => Some(Self::TaskbarCreated),
            4 => Some(Self::Suspend),
            5 => Some(Self::Resume),
            _ => None,
        }
    }

    const fn recycles_worker(self) -> bool {
        matches!(self, Self::TaskbarCreated | Self::Suspend | Self::Resume)
    }
}

const fn tray_status(paused: bool, worker_recovering: bool) -> TrayStatus {
    if paused {
        TrayStatus::Paused
    } else if worker_recovering {
        TrayStatus::WorkerRecovering
    } else {
        TrayStatus::Active
    }
}

pub(crate) struct MessageWindow {
    hwnd: HWND,
    dwell_timer: Option<WindowTimer>,
    preview_guard_timer: WindowTimer,
    hover_state: HoverState,
    preview_size: PreviewSize,
    input_diagnostics: Option<InputDiagnostics>,
    preview_diagnostics: Option<ActivePreviewDiagnostics>,
    performance_diagnostics: Option<ActivePerformanceDiagnostics>,
    preview_window: Option<PreviewWindow>,
    tray_icon: Option<TrayIcon>,
    settings_file: Option<SettingsFile>,
    settings_document: Option<SettingsDocument>,
    startup_registration: Option<StartupRegistration>,
    paused: bool,
    power_suspended: bool,
    worker_recovering: bool,
    worker_manager: Option<WorkerManager>,
    pending_worker_resolution: Option<PendingWorkerResolution>,
    pending_worker_anchor: Option<(Generation, PhysicalScreenPoint)>,
    latest_worker_completion: Option<Generation>,
    active_preview: Option<ActivePreview>,
    preview_event_hooks: Option<PreviewEventHooks>,
    raw_input: Option<RawInputRegistration>,
    _class: RegisteredWindowClass,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl MessageWindow {
    pub(crate) fn create() -> Result<Self> {
        Self::create_with_dwell_delay(DEFAULT_DWELL_DELAY)
    }

    pub(crate) fn create_with_dwell_delay(dwell_delay: Duration) -> Result<Self> {
        Self::create_with_preview_size(dwell_delay, PreviewSize::diagnostic())
    }

    pub(crate) fn create_for_application(
        dwell_delay: Duration,
        preview_size: PreviewSize,
    ) -> Result<Self> {
        Self::create_with_preview_size(dwell_delay, preview_size)
    }

    fn create_with_preview_size(dwell_delay: Duration, preview_size: PreviewSize) -> Result<Self> {
        // SAFETY: The static terminated string remains valid for this session-wide registration.
        let taskbar_created_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        if taskbar_created_message == 0 {
            return Err(Error::from_thread());
        }
        TASKBAR_CREATED_MESSAGE.store(taskbar_created_message, Ordering::Release);
        let class = RegisteredWindowClass::register()?;

        // SAFETY: The class remains registered in `class`, all string pointers are static, the
        // module instance belongs to this process, and no creation parameter is passed. Omitting
        // WS_VISIBLE creates a hidden top-level tool window that receives Shell/system broadcasts
        // without activation or a taskbar button.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                CLASS_NAME,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(class.instance),
                None,
            )?
        };

        let mut window = Self {
            hwnd,
            dwell_timer: Some(WindowTimer::new(hwnd, DWELL_TIMER_ID)),
            preview_guard_timer: WindowTimer::new(hwnd, PREVIEW_GUARD_TIMER_ID),
            hover_state: HoverState::new(dwell_delay),
            preview_size,
            input_diagnostics: None,
            preview_diagnostics: None,
            performance_diagnostics: None,
            preview_window: None,
            tray_icon: None,
            settings_file: None,
            settings_document: None,
            startup_registration: None,
            paused: false,
            power_suspended: false,
            worker_recovering: false,
            worker_manager: None,
            pending_worker_resolution: None,
            pending_worker_anchor: None,
            latest_worker_completion: None,
            active_preview: None,
            preview_event_hooks: Some(PreviewEventHooks::register()?),
            raw_input: None,
            _class: class,
            _thread_affinity: PhantomData,
        };
        window.raw_input = Some(RawInputRegistration::register(hwnd)?);

        Ok(window)
    }

    #[cfg(test)]
    pub(crate) fn request_shutdown(&self) -> Result<()> {
        // SAFETY: `self.hwnd` is owned by this live MessageWindow. The private message carries no
        // pointers or borrowed data, so its parameters remain valid until the queue processes it.
        unsafe { PostMessageW(Some(self.hwnd), SHUTDOWN_MESSAGE, WPARAM(0), LPARAM(0)) }
    }

    #[cfg(test)]
    pub(crate) fn run_message_loop(mut self) -> Result<()> {
        let _ = self.run_loop()?;
        Ok(())
    }

    pub(crate) fn run_application(
        mut self,
        worker_manager: WorkerManager,
        settings_file: SettingsFile,
        settings_document: SettingsDocument,
    ) -> std::result::Result<(), ApplicationRunError> {
        let startup_registration = StartupRegistration::for_current_executable()?;
        startup_registration.reconcile(settings_document.settings().start_with_windows())?;
        self.worker_manager = Some(worker_manager);
        self.settings_file = Some(settings_file);
        self.settings_document = Some(settings_document);
        self.startup_registration = Some(startup_registration);
        self.tray_icon = Some(TrayIcon::create(self.hwnd, TRAY_CALLBACK_MESSAGE)?);
        self.finish_application_loop()
    }

    #[cfg(test)]
    fn run_application_without_tray(
        mut self,
        worker_manager: WorkerManager,
    ) -> std::result::Result<(), ApplicationRunError> {
        self.worker_manager = Some(worker_manager);
        self.finish_application_loop()
    }

    fn finish_application_loop(mut self) -> std::result::Result<(), ApplicationRunError> {
        let loop_result = self.run_loop();
        let shutdown_result = self.shutdown_application();
        let exit = loop_result?;
        shutdown_result?;

        match exit {
            MessageLoopExit::Shutdown => Ok(()),
            MessageLoopExit::InputDiagnostics(_)
            | MessageLoopExit::PreviewDiagnostics(_)
            | MessageLoopExit::PerformanceDiagnostics(_) => {
                unreachable!("normal application mode cannot complete a diagnostic")
            }
        }
    }

    fn shutdown_application(&mut self) -> std::result::Result<(), WorkerManagerError> {
        self.paused = true;
        self.cancel_dwell();
        drop(self.preview_window.take());
        drop(self.tray_icon.take());

        if let Some(manager) = self.worker_manager.take() {
            manager.shutdown()?;
        }
        Ok(())
    }

    pub(crate) fn run_input_diagnostics(
        mut self,
        duration: Duration,
    ) -> Result<InputCoverageReport> {
        self.input_diagnostics = Some(InputDiagnostics::start(self.hwnd, duration)?);

        match self.run_loop()? {
            MessageLoopExit::Shutdown => Ok(self
                .input_diagnostics
                .as_mut()
                .map_or_else(InputCoverageReport::default, InputDiagnostics::finish)),
            MessageLoopExit::InputDiagnostics(report) => Ok(report),
            MessageLoopExit::PreviewDiagnostics(_) => {
                unreachable!("input-coverage diagnostics cannot run a preview-window diagnostic")
            }
            MessageLoopExit::PerformanceDiagnostics(_) => {
                unreachable!("input-coverage diagnostics cannot run a performance diagnostic")
            }
        }
    }

    pub(crate) fn run_preview_window_diagnostics(
        mut self,
        duration: Duration,
    ) -> Result<PreviewWindowDiagnosticReport> {
        let ui_task_started = Instant::now();
        let point = physical_cursor_position()?;
        let anchor = PhysicalScreenPoint::new(point.x, point.y);
        // SAFETY: This retrieves a borrowed window handle and transfers no ownership.
        let foreground_before = unsafe { GetForegroundWindow() };
        if foreground_before.0.is_null() {
            return Err(Error::from_thread());
        }

        let preview = PreviewWindow::create()?;
        let placement = preview.show_at(anchor)?;
        let mouse_activation_eaten = preview.eats_mouse_activation();
        // SAFETY: This is the same borrowed foreground-window query made after the no-activate
        // show and synchronous mouse-activation policy probe.
        let foreground_after = unsafe { GetForegroundWindow() };
        let focus_preserved =
            foreground_before == foreground_after && foreground_after.0 != preview.handle().0;

        let mut deadline_timer = WindowTimer::new(self.hwnd, PREVIEW_DIAGNOSTIC_DEADLINE_TIMER_ID);
        deadline_timer.arm(duration)?;
        self.preview_window = Some(preview);
        self.preview_diagnostics = Some(ActivePreviewDiagnostics {
            foreground_before,
            focus_preserved_at_show: focus_preserved,
            mouse_activation_eaten,
            placement,
            ui_thread_max: Duration::ZERO,
            _deadline_timer: deadline_timer,
        });
        self.record_ui_task(ui_task_started.elapsed());

        match self.run_loop()? {
            MessageLoopExit::PreviewDiagnostics(report) => Ok(report),
            MessageLoopExit::Shutdown => {
                Ok(self.finish_preview_diagnostics(PreviewWindowDismissal::Shutdown))
            }
            MessageLoopExit::InputDiagnostics(_) => {
                unreachable!("the preview-window diagnostic cannot run input-coverage diagnostics")
            }
            MessageLoopExit::PerformanceDiagnostics(_) => {
                unreachable!("the preview-window diagnostic cannot run a performance diagnostic")
            }
        }
    }

    fn run_loop(&mut self) -> Result<MessageLoopExit> {
        let mut message = MSG::default();

        loop {
            // SAFETY: `message` is valid writable storage for the duration of the call. No HWND or
            // range filter is used, so this thread's complete queue is serviced.
            let status = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if status.0 < 0 {
                return Err(Error::from_thread());
            }
            if status.0 == 0 {
                return Ok(MessageLoopExit::Shutdown);
            }
            // GetMessageW is the intended blocking wait. Start timing only after it returns so the
            // qualification counter measures one nonblocking UI-thread task, not idle time.
            let ui_task_started = Instant::now();

            if message.hwnd == self.hwnd && message.message == SHUTDOWN_MESSAGE {
                self.record_ui_task(ui_task_started.elapsed());
                return Ok(MessageLoopExit::Shutdown);
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == INPUT_DIAGNOSTIC_DEADLINE_TIMER_ID
            {
                let report = self
                    .input_diagnostics
                    .as_mut()
                    .map_or_else(InputCoverageReport::default, InputDiagnostics::finish);
                return Ok(MessageLoopExit::InputDiagnostics(report));
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == PREVIEW_DIAGNOSTIC_DEADLINE_TIMER_ID
            {
                self.record_ui_task(ui_task_started.elapsed());
                return Ok(MessageLoopExit::PreviewDiagnostics(
                    self.finish_preview_diagnostics(PreviewWindowDismissal::Timeout),
                ));
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == PERFORMANCE_DIAGNOSTIC_DEADLINE_TIMER_ID
            {
                self.record_ui_task(ui_task_started.elapsed());
                return Ok(MessageLoopExit::PerformanceDiagnostics(
                    self.finish_performance_diagnostics(),
                ));
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == INPUT_SAMPLE_TIMER_ID
            {
                self.handle_input_diagnostic_sample();
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == DWELL_TIMER_ID
            {
                self.handle_dwell_timer(Instant::now());
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == PREVIEW_GUARD_TIMER_ID
            {
                self.guard_product_preview();
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == WORKER_RESULT_MESSAGE {
                self.handle_worker_result();
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == TRAY_EVENT_MESSAGE {
                if self.handle_tray_callback(message.wParam, message.lParam)? {
                    return Ok(MessageLoopExit::Shutdown);
                }
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == ACTIVATE_MESSAGE {
                if self.handle_instance_activation()? {
                    return Ok(MessageLoopExit::Shutdown);
                }
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == SYSTEM_LIFECYCLE_CHANGED_MESSAGE {
                if let Some(change) =
                    SystemLifecycleChange::from_message_parameter(message.wParam.0)
                {
                    self.handle_system_lifecycle_change(change)?;
                }
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == PREVIEW_CONTEXT_INVALIDATED_MESSAGE {
                self.handle_preview_context_invalidation(message.wParam.0);
                self.record_ui_task(ui_task_started.elapsed());
                continue;
            }

            let raw_input = if message.hwnd == self.hwnd && message.message == WM_INPUT {
                Some(read_raw_input_activity(message.lParam))
            } else {
                None
            };

            // SAFETY: `message` was populated by a successful GetMessageW call and remains valid
            // through translation and synchronous dispatch on this owning thread.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }

            if let Some(raw_input) = raw_input {
                if preview_input_requires_dismissal(&raw_input)
                    && self.preview_diagnostics.is_some()
                {
                    self.record_ui_task(ui_task_started.elapsed());
                    return Ok(MessageLoopExit::PreviewDiagnostics(
                        self.finish_preview_diagnostics(PreviewWindowDismissal::Input),
                    ));
                }
                self.handle_raw_input(raw_input);
            }
            self.record_ui_task(ui_task_started.elapsed());
        }
    }

    fn record_ui_task(&mut self, duration: Duration) {
        if let Some(diagnostics) = self.preview_diagnostics.as_mut() {
            diagnostics.ui_thread_max = diagnostics.ui_thread_max.max(duration);
        }
        if let Some(diagnostics) = self.performance_diagnostics.as_mut() {
            diagnostics.ui_thread_max = diagnostics.ui_thread_max.max(duration);
        }
    }

    fn finish_preview_diagnostics(
        &mut self,
        dismissal: PreviewWindowDismissal,
    ) -> PreviewWindowDiagnosticReport {
        let finish_started = Instant::now();
        // SAFETY: This retrieves a borrowed window handle and transfers no ownership.
        let foreground_after_interaction = unsafe { GetForegroundWindow() };
        let preview_handle = self.preview_window.as_ref().map(PreviewWindow::handle);
        if let Some(preview) = self.preview_window.as_ref() {
            let _ = preview.hide();
        }

        let diagnostics = self
            .preview_diagnostics
            .take()
            .expect("a preview diagnostic exits only while active");
        let focus_preserved = diagnostics.focus_preserved_at_show
            && foreground_after_interaction == diagnostics.foreground_before
            && preview_handle.is_none_or(|handle| foreground_after_interaction != handle);
        let ui_thread_max = diagnostics.ui_thread_max.max(finish_started.elapsed());
        PreviewWindowDiagnosticReport {
            focus_preserved,
            mouse_activation_eaten: diagnostics.mouse_activation_eaten,
            placement: diagnostics.placement,
            dismissal,
            ui_thread_max,
        }
    }

    fn handle_raw_input(&mut self, raw_input: Result<Option<RawInputActivity>>) {
        if self.input_diagnostics.is_some() {
            self.handle_input_diagnostic_raw(raw_input);
            return;
        }
        if self.paused || self.power_suspended {
            return;
        }

        match raw_input {
            Ok(Some(RawInputActivity::Mouse(activity))) if activity.interrupted() => {
                self.cancel_dwell();
            }
            Ok(Some(RawInputActivity::Mouse(activity))) if activity.moved() => {
                match physical_cursor_position() {
                    Ok(point) => {
                        let point = PhysicalScreenPoint::new(point.x, point.y);
                        if pointer_motion_stays_on_active_target(self.active_preview, point)
                            && self.active_preview.is_some_and(|active| {
                                is_foreground_explorer_window_at(active.anchor)
                            })
                        {
                            return;
                        }
                        self.restart_dwell(point, Instant::now());
                    }
                    Err(_) => self.cancel_dwell(),
                }
            }
            Ok(Some(RawInputActivity::Mouse(_))) => {}
            Ok(Some(RawInputActivity::Keyboard)) => self.cancel_dwell(),
            Ok(None) | Err(_) => self.cancel_dwell(),
        }
    }

    fn handle_input_diagnostic_raw(&mut self, raw_input: Result<Option<RawInputActivity>>) {
        let activity = match raw_input {
            Ok(Some(RawInputActivity::Mouse(activity))) => activity,
            Ok(Some(RawInputActivity::Keyboard)) => return,
            Ok(None) | Err(_) => {
                self.suspend_input_diagnostics();
                return;
            }
        };
        if !activity.is_relevant() {
            return;
        }

        let Ok(point) = physical_cursor_position() else {
            self.suspend_input_diagnostics();
            return;
        };
        let point = PhysicalScreenPoint::new(point.x, point.y);
        if !is_foreground_explorer_window_at(point) {
            self.suspend_input_diagnostics();
            return;
        }

        let Some(diagnostics) = self.input_diagnostics.as_mut() else {
            return;
        };
        diagnostics
            .coverage
            .observe_raw(point, activity.moved(), activity.interrupted());
        if !diagnostics.sample_timer.is_armed()
            && diagnostics.sample_timer.arm(INPUT_SAMPLE_INTERVAL).is_err()
        {
            diagnostics.coverage.suspend();
        }
    }

    fn handle_input_diagnostic_sample(&mut self) {
        let Ok(point) = physical_cursor_position() else {
            self.suspend_input_diagnostics();
            return;
        };
        let point = PhysicalScreenPoint::new(point.x, point.y);
        if !is_foreground_explorer_window_at(point) {
            self.suspend_input_diagnostics();
            return;
        }

        if let Some(diagnostics) = self.input_diagnostics.as_mut() {
            diagnostics.coverage.observe_sample(point);
        }
    }

    fn suspend_input_diagnostics(&mut self) {
        if let Some(diagnostics) = self.input_diagnostics.as_mut() {
            diagnostics.coverage.suspend();
            let _ = diagnostics.sample_timer.stop();
        }
    }

    fn restart_dwell(&mut self, point: PhysicalScreenPoint, now: Instant) {
        self.invalidate_worker_delivery();
        let interval = self.hover_state.restart(point, now);
        let armed = self
            .dwell_timer
            .as_mut()
            .is_some_and(|timer| timer.arm(interval).is_ok());

        if !armed {
            self.cancel_dwell();
        }
    }

    fn cancel_dwell(&mut self) {
        self.invalidate_worker_delivery();
        self.hover_state.cancel();
        if let Some(timer) = self.dwell_timer.as_mut() {
            let _ = timer.stop();
        }
    }

    fn invalidate_worker_delivery(&mut self) {
        drop(self.pending_worker_resolution.take());
        self.pending_worker_anchor = None;
        self.latest_worker_completion = None;
        self.hide_product_preview();
    }

    fn handle_dwell_timer(&mut self, now: Instant) {
        if let Some(timer) = self.dwell_timer.as_mut() {
            let _ = timer.stop();
        }

        match self.hover_state.on_timer(now) {
            DwellTimerEvent::Inactive => {}
            DwellTimerEvent::Rearm(remaining) => {
                let armed = self
                    .dwell_timer
                    .as_mut()
                    .is_some_and(|timer| timer.arm(remaining).is_ok());
                if !armed {
                    self.cancel_dwell();
                }
            }
            DwellTimerEvent::Candidate(candidate) => {
                let ready = physical_cursor_position()
                    .map(|point| PhysicalScreenPoint::new(point.x, point.y))
                    .and_then(|current| {
                        let rectangle = system_hover_rectangle(candidate.anchor())?;
                        Ok(candidate.validate(current, rectangle))
                    })
                    .ok()
                    .flatten();

                let Some(ready) = ready else {
                    self.cancel_dwell();
                    return;
                };

                // Keep the validated generation attached through the manager, protocol, and UI
                // delivery boundaries so later input can invalidate this exact request.
                let (generation, point) = ready.into_parts();
                if !is_explorer_window_at(point) {
                    self.cancel_dwell();
                    return;
                }

                if !self.ensure_worker_manager_running() {
                    return;
                }
                let manager = self
                    .worker_manager
                    .as_ref()
                    .expect("a successful recovery check retains the worker manager");
                let notifier = worker_result_notifier(self.hwnd);
                let pending = match manager.submit_with_notifier(generation, point, notifier) {
                    Ok(pending) => pending,
                    Err(_) => {
                        self.set_worker_recovering(true);
                        return;
                    }
                };

                self.pending_worker_resolution = Some(pending);
                self.pending_worker_anchor = Some((generation, point));
            }
        }
    }

    fn handle_worker_result(&mut self) {
        let Some(pending) = self.pending_worker_resolution.as_mut() else {
            return;
        };

        match pending.poll() {
            PendingWorkerPoll::Pending => {}
            PendingWorkerPoll::Ready(result) => {
                self.pending_worker_resolution = None;
                let anchor = self.pending_worker_anchor.take();
                match result {
                    Ok(resolution) => {
                        self.set_worker_recovering(false);
                        let generation = accept_worker_completion(
                            self.hover_state.generation(),
                            Some(resolution.generation()),
                        );
                        self.latest_worker_completion = generation;

                        if let (Some(generation), Some((anchor_generation, point))) =
                            (generation, anchor)
                            && generation == anchor_generation
                        {
                            let (target_bounds, result) = resolution.into_parts();
                            if let Some(target_bounds) = target_bounds {
                                self.show_worker_result(point, target_bounds, result);
                            } else {
                                self.hide_product_preview();
                            }
                        } else {
                            self.hide_product_preview();
                        }
                    }
                    Err(error) => {
                        if !error.is_request_cancellation() {
                            self.set_worker_recovering(true);
                        }
                        self.latest_worker_completion = None;
                        self.hide_product_preview();
                    }
                }
            }
        }
    }

    fn ensure_worker_manager_running(&mut self) -> bool {
        let Some(manager) = self.worker_manager.as_mut() else {
            return false;
        };
        match manager.restart_if_stopped() {
            Ok(restarted) => {
                if restarted {
                    self.set_worker_recovering(true);
                }
                true
            }
            Err(_) => {
                self.set_worker_recovering(true);
                false
            }
        }
    }

    fn set_worker_recovering(&mut self, recovering: bool) {
        if self.worker_recovering == recovering {
            return;
        }
        self.worker_recovering = recovering;
        let status = tray_status(self.paused, self.worker_recovering);
        if let Some(tray) = self.tray_icon.as_mut() {
            let _ = tray.set_status(status);
        }
    }

    fn show_worker_result(
        &mut self,
        anchor: PhysicalScreenPoint,
        target_bounds: PhysicalScreenRect,
        result: PreviewResult,
    ) {
        if matches!(result, PreviewResult::Status(_)) {
            self.hide_product_preview();
            return;
        }
        if !target_bounds.contains(anchor) {
            self.hide_product_preview();
            return;
        }

        if self.preview_diagnostics.is_some() {
            return;
        }

        if self.preview_window.is_none() {
            let theme = self
                .settings_document
                .as_ref()
                .expect("normal preview creation requires loaded settings")
                .settings()
                .theme();
            let Ok(preview) = PreviewWindow::create_with_theme(theme) else {
                return;
            };
            self.preview_window = Some(preview);
        }

        let preview = self
            .preview_window
            .as_ref()
            .expect("the preview window was created above");
        let shown = match result {
            PreviewResult::Text(text) => preview.show_text_at(anchor, self.preview_size, &text),
            PreviewResult::Image(image) => preview.show_image_at(anchor, self.preview_size, image),
            PreviewResult::Status(_) => unreachable!("statuses returned before window creation"),
        };
        match shown {
            Ok(_) => {
                let Some(generation) = self.latest_worker_completion else {
                    self.hide_product_preview();
                    return;
                };
                let active = ActivePreview {
                    generation,
                    anchor,
                    target_bounds,
                };
                self.active_preview = Some(active);
                PreviewEventHooks::set_target(Some(PreviewEventTarget {
                    message_window: self.hwnd,
                    active,
                }));
                if self
                    .preview_guard_timer
                    .arm(PREVIEW_GUARD_INTERVAL)
                    .is_err()
                {
                    self.hide_product_preview();
                }
            }
            Err(_) => {
                self.clear_product_preview_state();
                drop(self.preview_window.take());
            }
        }
    }

    fn guard_product_preview(&mut self) {
        let Some(active) = self.active_preview else {
            let _ = self.preview_guard_timer.stop();
            return;
        };
        let current = physical_cursor_position()
            .ok()
            .map(|point| PhysicalScreenPoint::new(point.x, point.y));
        if !preview_context_is_current(
            active.target_bounds,
            current,
            is_foreground_explorer_window_at(active.anchor),
        ) {
            self.cancel_dwell();
        }
    }

    fn handle_preview_context_invalidation(&mut self, generation: usize) {
        if preview_context_generation_matches(self.active_preview, generation) {
            self.cancel_dwell();
        }
    }

    fn handle_system_lifecycle_change(&mut self, change: SystemLifecycleChange) -> Result<()> {
        // Display topology, power transitions, work-area changes, and taskbar recreation can
        // invalidate both the physical anchor and the Explorer UIA/Shell context. Require fresh
        // input before another preview.
        self.cancel_dwell();

        match change {
            SystemLifecycleChange::Suspend | SystemLifecycleChange::Resume => {
                self.power_suspended = change == SystemLifecycleChange::Suspend;
            }
            SystemLifecycleChange::TaskbarCreated => {
                if let Some(tray) = self.tray_icon.as_mut() {
                    tray.restore_after_taskbar_created()?;
                }
            }
            SystemLifecycleChange::DisplayConfiguration | SystemLifecycleChange::WorkArea => {}
        }

        if change.recycles_worker() {
            let recycle = self
                .worker_manager
                .as_ref()
                .map(WorkerManager::request_session_recycle);
            if matches!(recycle, Some(Err(_))) {
                self.set_worker_recovering(true);
            }
        }
        Ok(())
    }

    fn hide_product_preview(&mut self) {
        self.clear_product_preview_state();
        if self.preview_diagnostics.is_none()
            && let Some(preview) = self.preview_window.as_ref()
        {
            let _ = preview.hide();
        }
    }

    fn clear_product_preview_state(&mut self) {
        self.active_preview = None;
        PreviewEventHooks::set_target(None);
        let _ = self.preview_guard_timer.stop();
    }

    fn handle_tray_callback(&mut self, wparam: WPARAM, lparam: LPARAM) -> Result<bool> {
        let state = self.tray_menu_state();
        let command = self
            .tray_icon
            .as_ref()
            .map(|tray| tray.command_for_callback(wparam, lparam, state))
            .transpose()?
            .flatten();
        self.apply_tray_command(command)
    }

    fn handle_instance_activation(&mut self) -> Result<bool> {
        let state = self.tray_menu_state();
        let command = self
            .tray_icon
            .as_ref()
            .map(|tray| tray.command_at_cursor(state))
            .transpose()?
            .flatten();
        self.apply_tray_command(command)
    }

    fn apply_tray_command(&mut self, command: Option<TrayCommand>) -> Result<bool> {
        match command {
            None => Ok(false),
            Some(TrayCommand::TogglePaused) => {
                let paused = !self.paused;
                let status = tray_status(paused, self.worker_recovering);
                self.tray_icon
                    .as_mut()
                    .expect("a tray command requires the live tray owner")
                    .set_status(status)?;
                self.paused = paused;
                if paused {
                    self.cancel_dwell();
                }
                Ok(false)
            }
            Some(TrayCommand::SetDwellDelay(dwell_delay_ms)) => {
                self.update_dwell_delay(dwell_delay_ms);
                Ok(false)
            }
            Some(TrayCommand::SetPreviewSize(width, height)) => {
                self.update_preview_size(width, height);
                Ok(false)
            }
            Some(TrayCommand::SetTheme(theme)) => {
                self.update_theme(theme);
                Ok(false)
            }
            Some(TrayCommand::ToggleStartWithWindows) => {
                self.toggle_start_with_windows();
                Ok(false)
            }
            Some(TrayCommand::About) => {
                self.tray_icon
                    .as_ref()
                    .expect("a tray command requires the live tray owner")
                    .show_about();
                Ok(false)
            }
            Some(TrayCommand::Exit) => Ok(true),
        }
    }

    fn tray_menu_state(&self) -> TrayMenuState {
        let settings = self
            .settings_document
            .as_ref()
            .expect("a live tray requires the application settings")
            .settings();
        TrayMenuState {
            paused: self.paused,
            dwell_delay_ms: settings.dwell_delay_ms(),
            preview_width: settings.preview_width(),
            preview_height: settings.preview_height(),
            theme: settings.theme(),
            start_with_windows: settings.start_with_windows(),
        }
    }

    fn update_dwell_delay(&mut self, dwell_delay_ms: u64) {
        let mut updated = self
            .settings_document
            .as_ref()
            .expect("a settings command requires the loaded document")
            .clone();
        if let Err(error) = updated.set_dwell_delay_ms(dwell_delay_ms) {
            self.show_settings_error("The dwell delay was not changed.", &error.to_string());
            return;
        }
        if !self.save_settings(&updated) {
            return;
        }

        self.settings_document = Some(updated);
        self.cancel_dwell();
        self.hover_state
            .set_delay(Duration::from_millis(dwell_delay_ms));
    }

    fn update_preview_size(&mut self, width: u16, height: u16) {
        let mut updated = self
            .settings_document
            .as_ref()
            .expect("a settings command requires the loaded document")
            .clone();
        if let Err(error) = updated.set_preview_size(width, height) {
            self.show_settings_error("The preview size was not changed.", &error.to_string());
            return;
        }
        if !self.save_settings(&updated) {
            return;
        }

        self.settings_document = Some(updated);
        self.cancel_dwell();
        self.preview_size = PreviewSize::new(u32::from(width), u32::from(height));
    }

    fn update_theme(&mut self, theme: Theme) {
        let mut updated = self
            .settings_document
            .as_ref()
            .expect("a settings command requires the loaded document")
            .clone();
        updated.set_theme(theme);
        if !self.save_settings(&updated) {
            return;
        }

        self.settings_document = Some(updated);
        self.cancel_dwell();
        if let Some(preview) = self.preview_window.as_ref()
            && let Err(error) = preview.set_theme(theme)
        {
            drop(self.preview_window.take());
            self.show_settings_error(
                "The theme was saved, but the preview could not be refreshed.",
                &error.to_string(),
            );
        }
    }

    fn toggle_start_with_windows(&mut self) {
        let current = self
            .settings_document
            .as_ref()
            .expect("a settings command requires the loaded document")
            .settings()
            .start_with_windows();
        let desired = !current;
        let registration = self
            .startup_registration
            .as_ref()
            .expect("a startup command requires the current executable registration");

        if let Err(error) = registration.set_enabled(desired) {
            self.show_settings_error(
                "The Start with Windows setting was not changed.",
                &error.to_string(),
            );
            return;
        }

        let mut updated = self
            .settings_document
            .as_ref()
            .expect("a settings command requires the loaded document")
            .clone();
        updated.set_start_with_windows(desired);
        let save_result = self
            .settings_file
            .as_ref()
            .expect("a settings command requires the discovered settings file")
            .save(&updated);
        if let Err(error) = save_result {
            let rollback = registration.set_enabled(current);
            let detail = match rollback {
                Ok(()) => error.to_string(),
                Err(rollback_error) => format!(
                    "{error}\r\n\r\nThe startup registration could not be rolled back: \
                     {rollback_error}"
                ),
            };
            self.show_settings_error("The Start with Windows setting was not saved.", &detail);
            return;
        }

        self.settings_document = Some(updated);
    }

    fn save_settings(&self, updated: &SettingsDocument) -> bool {
        match self
            .settings_file
            .as_ref()
            .expect("a settings command requires the discovered settings file")
            .save(updated)
        {
            Ok(()) => true,
            Err(error) => {
                self.show_settings_error("The settings file was not updated.", &error.to_string());
                false
            }
        }
    }

    fn show_settings_error(&self, summary: &str, detail: &str) {
        self.tray_icon
            .as_ref()
            .expect("a settings command requires the live tray owner")
            .show_error(&format!("{summary}\r\n\r\n{detail}"));
    }

    #[cfg(test)]
    fn handle(&self) -> HWND {
        self.hwnd
    }

    #[cfg(test)]
    fn dwell_timer_is_armed(&self) -> bool {
        self.dwell_timer.as_ref().is_some_and(WindowTimer::is_armed)
    }

    #[cfg(test)]
    fn dwell_delay(&self) -> Duration {
        self.hover_state.delay()
    }
}

#[derive(Debug)]
pub(crate) enum ApplicationRunError {
    Windows(Error),
    WorkerManager(WorkerManagerError),
}

impl fmt::Display for ApplicationRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Windows(error) => write!(formatter, "{error}"),
            Self::WorkerManager(error) => write!(formatter, "worker manager: {error}"),
        }
    }
}

impl StdError for ApplicationRunError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Windows(error) => Some(error),
            Self::WorkerManager(error) => Some(error),
        }
    }
}

impl From<Error> for ApplicationRunError {
    fn from(error: Error) -> Self {
        Self::Windows(error)
    }
}

impl From<WorkerManagerError> for ApplicationRunError {
    fn from(error: WorkerManagerError) -> Self {
        Self::WorkerManager(error)
    }
}

fn worker_result_notifier(hwnd: HWND) -> CompletionNotifier {
    CompletionNotifier::new(hwnd.0 as usize, post_worker_result)
}

fn post_worker_result(raw_hwnd: usize) {
    let hwnd = HWND(raw_hwnd as *mut core::ffi::c_void);
    // SAFETY: The callback carries only the numeric value of the application-owned HWND and posts
    // a parameter-free private message. Normal shutdown joins the worker manager before destroying
    // the HWND; a late post during teardown is allowed to fail without dereferencing the stale
    // value.
    let _ = unsafe { PostMessageW(Some(hwnd), WORKER_RESULT_MESSAGE, WPARAM(0), LPARAM(0)) };
}

fn accept_worker_completion(
    current: Generation,
    completed: Option<Generation>,
) -> Option<Generation> {
    completed.filter(|generation| *generation == current)
}

fn preview_context_is_current(
    target_bounds: PhysicalScreenRect,
    current: Option<PhysicalScreenPoint>,
    foreground_explorer: bool,
) -> bool {
    current.is_some_and(|point| target_bounds.contains(point)) && foreground_explorer
}

fn pointer_motion_stays_on_active_target(
    active: Option<ActivePreview>,
    current: PhysicalScreenPoint,
) -> bool {
    active.is_some_and(|active| active.target_bounds.contains(current))
}

fn preview_context_generation_matches(active: Option<ActivePreview>, generation: usize) -> bool {
    active.is_some_and(|active| usize::try_from(active.generation.get()) == Ok(generation))
}

fn preview_input_requires_dismissal(raw_input: &Result<Option<RawInputActivity>>) -> bool {
    match raw_input {
        Ok(Some(RawInputActivity::Mouse(activity))) => activity.is_relevant(),
        Ok(Some(RawInputActivity::Keyboard)) => true,
        Ok(None) | Err(_) => true,
    }
}

#[derive(Clone, Copy)]
struct ActivePreview {
    generation: Generation,
    anchor: PhysicalScreenPoint,
    target_bounds: PhysicalScreenRect,
}

#[derive(Clone, Copy)]
struct PreviewEventTarget {
    message_window: HWND,
    active: ActivePreview,
}

thread_local! {
    static PREVIEW_EVENT_TARGET: Cell<Option<PreviewEventTarget>> = const { Cell::new(None) };
}

struct PreviewEventHooks {
    hooks: [HWINEVENTHOOK; 2],
    _thread_affinity: PhantomData<Rc<()>>,
}

impl PreviewEventHooks {
    fn register() -> Result<Self> {
        let foreground =
            register_preview_event_hook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND)?;
        let object =
            match register_preview_event_hook(EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE) {
                Ok(hook) => hook,
                Err(error) => {
                    // SAFETY: `foreground` is the live hook returned above and has not been unhooked.
                    unsafe {
                        let _ = UnhookWinEvent(foreground);
                    }
                    return Err(error);
                }
            };

        Ok(Self {
            hooks: [foreground, object],
            _thread_affinity: PhantomData,
        })
    }

    fn set_target(target: Option<PreviewEventTarget>) {
        PREVIEW_EVENT_TARGET.set(target);
    }
}

impl Drop for PreviewEventHooks {
    fn drop(&mut self) {
        Self::set_target(None);
        for hook in self.hooks {
            // SAFETY: Each handle was returned by SetWinEventHook and is unhooked exactly once on
            // the registering UI thread before the message-window target is destroyed.
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
    }
}

fn register_preview_event_hook(event_min: u32, event_max: u32) -> Result<HWINEVENTHOOK> {
    // SAFETY: The callback is a static system-ABI function. Out-of-context delivery returns to
    // this registering thread's message loop, skips events from CursorPeek itself, and supplies no
    // module handle because the callback is not injected into another process.
    let hook = unsafe {
        SetWinEventHook(
            event_min,
            event_max,
            None,
            Some(preview_event_callback),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.is_invalid() {
        Err(Error::from_thread())
    } else {
        Ok(hook)
    }
}

unsafe extern "system" fn preview_event_callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    event_window: HWND,
    _object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        PREVIEW_EVENT_TARGET.with(|target| {
            let Some(target) = target.get() else {
                return;
            };
            if event != EVENT_SYSTEM_FOREGROUND
                && !belongs_to_explorer_window_at(event_window, target.active.anchor)
            {
                return;
            }
            let Ok(generation) = usize::try_from(target.active.generation.get()) else {
                return;
            };

            // SAFETY: The target is the live hidden coordinator HWND owned by the registering
            // thread. The private message carries only a generation scalar, never a callback
            // pointer or borrowed event data.
            unsafe {
                let _ = PostMessageW(
                    Some(target.message_window),
                    PREVIEW_CONTEXT_INVALIDATED_MESSAGE,
                    WPARAM(generation),
                    LPARAM(0),
                );
            }
        });
    }));
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        drop(self.tray_icon.take());
        drop(self.preview_diagnostics.take());
        drop(self.performance_diagnostics.take());
        drop(self.preview_window.take());
        drop(self.input_diagnostics.take());
        drop(self.dwell_timer.take());
        let _ = self.preview_guard_timer.stop();
        drop(self.pending_worker_resolution.take());
        drop(self.worker_manager.take());
        drop(self.preview_event_hooks.take());
        drop(self.raw_input.take());

        // SAFETY: The owner is !Send and therefore drops on the creating thread. All timers have
        // been stopped and Raw Input unregistered. The HWND returned by CreateWindowExW is
        // destroyed before `_class` is dropped/unregistered.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

enum MessageLoopExit {
    Shutdown,
    InputDiagnostics(InputCoverageReport),
    PreviewDiagnostics(PreviewWindowDiagnosticReport),
    PerformanceDiagnostics(performance::PerformanceDiagnosticReport),
}

struct ActivePreviewDiagnostics {
    foreground_before: HWND,
    focus_preserved_at_show: bool,
    mouse_activation_eaten: bool,
    placement: PreviewPlacement,
    ui_thread_max: Duration,
    _deadline_timer: WindowTimer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreviewWindowDismissal {
    Input,
    Timeout,
    Shutdown,
}

pub(crate) struct PreviewWindowDiagnosticReport {
    focus_preserved: bool,
    mouse_activation_eaten: bool,
    placement: PreviewPlacement,
    dismissal: PreviewWindowDismissal,
    ui_thread_max: Duration,
}

impl fmt::Display for PreviewWindowDiagnosticReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let focus = if self.focus_preserved { "yes" } else { "no" };
        let mouse_activation = if self.mouse_activation_eaten {
            "eaten"
        } else {
            "passed"
        };
        let dismissal = match self.dismissal {
            PreviewWindowDismissal::Input => "input",
            PreviewWindowDismissal::Timeout => "timeout",
            PreviewWindowDismissal::Shutdown => "shutdown",
        };
        let ui_thread_max_us = u64::try_from(self.ui_thread_max.as_micros()).unwrap_or(u64::MAX);

        write!(
            formatter,
            "No-activate preview diagnostic completed: focus_preserved={focus}, \
             mouse_activation={mouse_activation}, dismissal={dismissal}, x={}, y={}, width={}, \
             height={}, inside_work_area=yes, pointer_gap_preserved=yes, ui_thread_max_us={}",
            self.placement.x,
            self.placement.y,
            self.placement.width,
            self.placement.height,
            ui_thread_max_us
        )
    }
}

struct InputDiagnostics {
    coverage: InputCoverage,
    sample_timer: WindowTimer,
    deadline_timer: WindowTimer,
}

impl InputDiagnostics {
    fn start(hwnd: HWND, duration: Duration) -> Result<Self> {
        let mut diagnostics = Self {
            coverage: InputCoverage::default(),
            sample_timer: WindowTimer::new(hwnd, INPUT_SAMPLE_TIMER_ID),
            deadline_timer: WindowTimer::new(hwnd, INPUT_DIAGNOSTIC_DEADLINE_TIMER_ID),
        };
        diagnostics.deadline_timer.arm(duration)?;
        Ok(diagnostics)
    }

    fn finish(&mut self) -> InputCoverageReport {
        let _ = self.sample_timer.stop();
        let _ = self.deadline_timer.stop();
        self.coverage.suspend();
        self.coverage.report()
    }
}

struct WindowTimer {
    hwnd: HWND,
    id: usize,
    armed: bool,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl WindowTimer {
    fn new(hwnd: HWND, id: usize) -> Self {
        debug_assert_ne!(id, 0);

        Self {
            hwnd,
            id,
            armed: false,
            _thread_affinity: PhantomData,
        }
    }

    fn arm(&mut self, interval: Duration) -> Result<()> {
        let interval_ms = timer_interval_ms(interval);

        // SAFETY: `hwnd` is a live window owned by this calling UI thread. The fixed nonzero ID
        // belongs only to that window, the interval is clamped to Windows' documented range, and
        // no callback pointer is supplied, so expiry is delivered as WM_TIMER.
        let timer = unsafe { SetTimer(Some(self.hwnd), self.id, interval_ms, None) };
        if timer == 0 {
            let error = Error::from_thread();
            let _ = self.stop();
            return Err(error);
        }

        self.armed = true;
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if !self.armed {
            return Ok(());
        }

        // SAFETY: This uses the same live owning HWND and fixed ID passed to SetTimer. The token
        // never leaves the UI thread and retries in Drop if Windows reports a failure.
        let result = unsafe { KillTimer(Some(self.hwnd), self.id) };
        if result.is_ok() {
            self.armed = false;
        }
        result
    }

    fn is_armed(&self) -> bool {
        self.armed
    }
}

impl Drop for WindowTimer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn timer_interval_ms(interval: Duration) -> u32 {
    let rounded_up_ms = interval.as_nanos().div_ceil(1_000_000);
    rounded_up_ms.clamp(
        u128::from(USER_TIMER_MINIMUM),
        u128::from(USER_TIMER_MAXIMUM),
    ) as u32
}

struct RegisteredWindowClass {
    instance: HINSTANCE,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl RegisteredWindowClass {
    fn register() -> Result<Self> {
        // SAFETY: A null module name asks for the current executable module. The returned handle
        // is borrowed and is deliberately stored as a plain HINSTANCE, never as Owned.
        let instance = HINSTANCE::from(unsafe { GetModuleHandleW(None)? });
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(message_window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };

        // SAFETY: `window_class` is fully initialized, its string and callback pointers remain
        // valid for the registration lifetime, and registration occurs on the owning thread.
        let atom = unsafe { RegisterClassW(&window_class) };
        if atom == 0 {
            return Err(Error::from_thread());
        }

        Ok(Self {
            instance,
            _thread_affinity: PhantomData,
        })
    }
}

impl Drop for RegisteredWindowClass {
    fn drop(&mut self) {
        // SAFETY: MessageWindow destroys its only HWND before this field is dropped. The class
        // name and borrowed process instance are the same values used during registration.
        unsafe {
            let _ = UnregisterClassW(CLASS_NAME, Some(self.instance));
        }
    }
}

unsafe extern "system" fn message_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    catch_unwind(AssertUnwindSafe(|| {
        dispatch_message(hwnd, message, wparam, lparam)
    }))
    .unwrap_or(LRESULT(0))
}

fn dispatch_message(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    #[cfg(test)]
    if message == TEST_PANIC_MESSAGE {
        panic!("intentional callback panic for containment testing");
    }

    if message == TRAY_CALLBACK_MESSAGE {
        // Shell notification callbacks can enter WNDPROC synchronously instead of appearing as
        // removable queue entries. Copy the scalar version-4 callback into the product event
        // queue so menu creation and settings mutation remain inside MessageWindow::run_loop.
        // SAFETY: The target is the live window currently executing this procedure, and the
        // version-4 callback parameters are scalar values with no borrowed pointer lifetime.
        let _ = unsafe { PostMessageW(Some(hwnd), TRAY_EVENT_MESSAGE, wparam, lparam) };
        return LRESULT(0);
    }

    if let Some(change) = system_lifecycle_change_for_message(message, wparam) {
        // SAFETY: Broadcasts and registered Shell messages enter WNDPROC synchronously. This
        // pointer-free private message targets the same live HWND and defers all product state
        // changes to its normal queue.
        let _ = unsafe {
            PostMessageW(
                Some(hwnd),
                SYSTEM_LIFECYCLE_CHANGED_MESSAGE,
                WPARAM(change as usize),
                LPARAM(0),
            )
        };
        return if message == WM_POWERBROADCAST {
            LRESULT(1)
        } else {
            LRESULT(0)
        };
    }

    // SAFETY: These are the untouched parameters supplied by Windows to this window procedure.
    // Every WM_INPUT reaches this default procedure because the owning loop copies it before
    // dispatch and applies safe state only after required foreground cleanup.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn system_lifecycle_change_for_message(
    message: u32,
    wparam: WPARAM,
) -> Option<SystemLifecycleChange> {
    if message == WM_DISPLAYCHANGE {
        return Some(SystemLifecycleChange::DisplayConfiguration);
    }
    if message == WM_SETTINGCHANGE && wparam.0 == SPI_SETWORKAREA.0 as usize {
        return Some(SystemLifecycleChange::WorkArea);
    }
    if message == WM_POWERBROADCAST {
        return match wparam.0 as u32 {
            PBT_APMSUSPEND => Some(SystemLifecycleChange::Suspend),
            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMECRITICAL | PBT_APMRESUMESUSPEND => {
                Some(SystemLifecycleChange::Resume)
            }
            _ => None,
        };
    }

    let taskbar_created = TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire);
    (taskbar_created != 0 && message == taskbar_created)
        .then_some(SystemLifecycleChange::TaskbarCreated)
}

#[cfg(test)]
mod tests {
    use super::{
        ActivePreview, CLASS_NAME, DWELL_TIMER_ID, IsWindow, LPARAM, MessageWindow,
        PREVIEW_WINDOW_DIAGNOSTIC_DURATION, PREVIEW_WINDOW_PRACTICE_DURATION,
        SYSTEM_LIFECYCLE_CHANGED_MESSAGE, SystemLifecycleChange, TASKBAR_CREATED_MESSAGE,
        TEST_PANIC_MESSAGE, TRAY_CALLBACK_MESSAGE, TRAY_EVENT_MESSAGE, WPARAM,
        accept_worker_completion, pointer_motion_stays_on_active_target, post_worker_result,
        preview_context_generation_matches, preview_context_is_current,
        preview_input_requires_dismissal, registered_raw_devices, timer_interval_ms, tray_status,
    };
    use crate::hover::{Generation, PhysicalScreenPoint};
    use crate::platform::windows::TrayStatus;
    use crate::platform::windows::explorer::is_explorer_window;
    use crate::platform::windows::input::{RawInputActivity, RawMouseActivity};
    use crate::worker::WorkerManager;
    use cursorpeek_core::PhysicalScreenRect;
    use std::{
        sync::atomic::Ordering,
        thread,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::UI::{
            Input::RIDEV_INPUTSINK,
            WindowsAndMessaging::{
                FindWindowExW, HWND_MESSAGE, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND,
                PM_REMOVE, PeekMessageW, PostMessageW, SPI_SETWORKAREA, SendMessageW,
                WM_DISPLAYCHANGE, WM_POWERBROADCAST, WM_SETTINGCHANGE, WM_TIMER,
            },
        },
        core::{Error, PCWSTR},
    };

    #[test]
    fn worker_delivery_accepts_only_the_current_generation() {
        let current = Generation::from_raw(8);

        assert_eq!(
            accept_worker_completion(current, Some(Generation::from_raw(7))),
            None,
            "a delayed response must not cross a newer input generation"
        );
        assert_eq!(
            accept_worker_completion(current, Some(Generation::from_raw(9))),
            None,
            "an out-of-order future response must fail closed"
        );
        assert_eq!(
            accept_worker_completion(current, None),
            None,
            "worker errors cannot become an accepted completion"
        );
        assert_eq!(
            accept_worker_completion(current, Some(current)),
            Some(current)
        );
    }

    #[test]
    fn tray_status_prioritizes_pause_and_exposes_worker_recovery() {
        assert_eq!(tray_status(false, false), TrayStatus::Active);
        assert_eq!(tray_status(false, true), TrayStatus::WorkerRecovering);
        assert_eq!(tray_status(true, false), TrayStatus::Paused);
        assert_eq!(tray_status(true, true), TrayStatus::Paused);
    }

    #[test]
    fn shell_and_power_lifecycle_changes_recycle_the_worker() {
        assert!(SystemLifecycleChange::TaskbarCreated.recycles_worker());
        assert!(SystemLifecycleChange::Suspend.recycles_worker());
        assert!(SystemLifecycleChange::Resume.recycles_worker());
        assert!(!SystemLifecycleChange::DisplayConfiguration.recycles_worker());
        assert!(!SystemLifecycleChange::WorkArea.recycles_worker());
    }

    #[test]
    fn message_window_lifecycle_and_callback_boundary_are_sound() {
        thread::spawn(|| {
            let mut first =
                MessageWindow::create().expect("the hidden coordinator should be created");
            let first_handle = first.handle();

            // SAFETY: `first_handle` belongs to the live window on this test thread.
            assert!(unsafe { IsWindow(Some(first_handle)).as_bool() });
            // SAFETY: The exact class is process-private. The null-parent search finds the hidden
            // top-level coordinator.
            let top_level = unsafe { FindWindowExW(None, None, CLASS_NAME, PCWSTR::null()) }
                .expect("the top-level coordinator should be discoverable");
            assert_eq!(top_level, first_handle);
            // SAFETY: The same exact class lookup is read-only; searching HWND_MESSAGE verifies the
            // coordinator was not created in the message-only hierarchy.
            let message_only =
                unsafe { FindWindowExW(Some(HWND_MESSAGE), None, CLASS_NAME, PCWSTR::null()) };
            assert!(
                message_only.is_err(),
                "the coordinator must not remain message-only because broadcasts would be lost"
            );
            assert!(
                !is_explorer_window(first_handle),
                "the private coordinator must fail the Explorer candidate gate"
            );
            assert_raw_input_registrations(first_handle);

            let tray_wparam = WPARAM(0x0123_4567);
            let tray_lparam = LPARAM(0x7654_0321);
            // SAFETY: The synthetic notification carries only the same scalar values used by the
            // Shell's version-4 callback contract and targets the live coordinator directly.
            let result = unsafe {
                SendMessageW(
                    first_handle,
                    TRAY_CALLBACK_MESSAGE,
                    Some(tray_wparam),
                    Some(tray_lparam),
                )
            };
            assert_eq!(result.0, 0);
            let mut tray_event = MSG::default();
            // SAFETY: `tray_event` is writable storage and the exact private filter removes only
            // the callback copy posted by CursorPeek's own WNDPROC.
            let found = unsafe {
                PeekMessageW(
                    &mut tray_event,
                    Some(first_handle),
                    TRAY_EVENT_MESSAGE,
                    TRAY_EVENT_MESSAGE,
                    PM_REMOVE,
                )
            };
            assert!(found.as_bool());
            assert_eq!(tray_event.wParam, tray_wparam);
            assert_eq!(tray_event.lParam, tray_lparam);

            let taskbar_created = TASKBAR_CREATED_MESSAGE.load(Ordering::Acquire);
            assert_ne!(taskbar_created, 0);
            for (message, parameter, expected, expected_result) in [
                (
                    WM_DISPLAYCHANGE,
                    WPARAM(32),
                    SystemLifecycleChange::DisplayConfiguration,
                    0,
                ),
                (
                    WM_SETTINGCHANGE,
                    WPARAM(SPI_SETWORKAREA.0 as usize),
                    SystemLifecycleChange::WorkArea,
                    0,
                ),
                (
                    taskbar_created,
                    WPARAM(0),
                    SystemLifecycleChange::TaskbarCreated,
                    0,
                ),
                (
                    WM_POWERBROADCAST,
                    WPARAM(PBT_APMSUSPEND as usize),
                    SystemLifecycleChange::Suspend,
                    1,
                ),
                (
                    WM_POWERBROADCAST,
                    WPARAM(PBT_APMRESUMEAUTOMATIC as usize),
                    SystemLifecycleChange::Resume,
                    1,
                ),
            ] {
                // SAFETY: These synchronous, pointer-free messages target the live coordinator
                // directly rather than broadcasting into other applications.
                let result = unsafe { SendMessageW(first_handle, message, Some(parameter), None) };
                assert_eq!(result.0, expected_result);
                let mut queued = MSG::default();
                // SAFETY: queued is writable storage; the exact private-message filter removes
                // only CursorPeek's reduced lifecycle event from this owning thread's queue.
                let found = unsafe {
                    PeekMessageW(
                        &mut queued,
                        Some(first_handle),
                        SYSTEM_LIFECYCLE_CHANGED_MESSAGE,
                        SYSTEM_LIFECYCLE_CHANGED_MESSAGE,
                        PM_REMOVE,
                    )
                };
                assert!(found.as_bool());
                assert_eq!(
                    SystemLifecycleChange::from_message_parameter(queued.wParam.0),
                    Some(expected)
                );
            }

            // Unrelated settings broadcasts still receive default processing and do not cancel
            // product state through the private lifecycle path.
            // SAFETY: This synchronous pointer-free message targets the live coordinator owned by
            // the current test thread.
            unsafe { SendMessageW(first_handle, WM_SETTINGCHANGE, Some(WPARAM(0)), None) };
            let mut unrelated = MSG::default();
            // SAFETY: `unrelated` is writable and the exact private filter only inspects this
            // thread's live coordinator queue.
            let found = unsafe {
                PeekMessageW(
                    &mut unrelated,
                    Some(first_handle),
                    SYSTEM_LIFECYCLE_CHANGED_MESSAGE,
                    SYSTEM_LIFECYCLE_CHANGED_MESSAGE,
                    PM_REMOVE,
                )
            };
            assert!(!found.as_bool());

            // Unknown or pointer-bearing power events stay with DefWindowProc and are never
            // copied into CursorPeek's scalar lifecycle queue.
            // SAFETY: The deliberately unknown scalar parameter carries no pointer and is sent
            // synchronously to the live coordinator on this test thread.
            unsafe {
                SendMessageW(
                    first_handle,
                    WM_POWERBROADCAST,
                    Some(WPARAM(u32::MAX as usize)),
                    None,
                )
            };
            // SAFETY: `unrelated` remains writable and the private-message filter only reads this
            // thread's live coordinator queue.
            let found = unsafe {
                PeekMessageW(
                    &mut unrelated,
                    Some(first_handle),
                    SYSTEM_LIFECYCLE_CHANGED_MESSAGE,
                    SYSTEM_LIFECYCLE_CHANGED_MESSAGE,
                    PM_REMOVE,
                )
            };
            assert!(!found.as_bool());

            first.restart_dwell(PhysicalScreenPoint::new(-10, 20), Instant::now());
            assert!(first.dwell_timer_is_armed());
            first
                .handle_system_lifecycle_change(SystemLifecycleChange::DisplayConfiguration)
                .expect("display invalidation without a tray should be infallible");
            assert!(
                !first.dwell_timer_is_armed(),
                "display changes must cancel stale physical hover work"
            );
            first
                .handle_system_lifecycle_change(SystemLifecycleChange::Suspend)
                .expect("suspend invalidation without a worker should be infallible");
            assert!(first.power_suspended);
            first
                .handle_system_lifecycle_change(SystemLifecycleChange::Resume)
                .expect("resume invalidation without a worker should be infallible");
            assert!(!first.power_suspended);
            first.restart_dwell(PhysicalScreenPoint::new(-10, 20), Instant::now());
            first.handle_dwell_timer(Instant::now());
            assert!(
                first.dwell_timer_is_armed(),
                "an early timer must re-arm the remaining dwell"
            );

            // SAFETY: This posts an early message using the live window's owned timer ID and no
            // callback pointer. The monotonic state must re-arm the remaining dwell instead of
            // treating message arrival alone as expiry.
            unsafe {
                PostMessageW(
                    Some(first_handle),
                    WM_TIMER,
                    WPARAM(DWELL_TIMER_ID),
                    LPARAM(0),
                )
            }
            .expect("the early timer message should be queued");

            // SAFETY: The live window owns the receiving queue and this private message carries
            // only zero-valued parameters. Dispatch deliberately panics inside the WNDPROC's
            // catch_unwind boundary.
            unsafe { PostMessageW(Some(first_handle), TEST_PANIC_MESSAGE, WPARAM(0), LPARAM(0)) }
                .expect("the callback test message should be queued");
            post_worker_result(first_handle.0 as usize);
            first
                .request_shutdown()
                .expect("the shutdown message should be queued");
            first
                .run_message_loop()
                .expect("the queued messages should be pumped");

            // SAFETY: Checking a stale HWND with IsWindow is the documented validity probe.
            assert!(!unsafe { IsWindow(Some(first_handle)).as_bool() });
            assert!(
                registered_raw_devices()
                    .expect("the process registration should be queryable")
                    .is_empty(),
                "raw input should be unregistered before window teardown"
            );

            let application = MessageWindow::create_with_dwell_delay(Duration::from_millis(650))
                .expect("the application message window should be created");
            assert_eq!(application.dwell_delay(), Duration::from_millis(650));
            let application_handle = application.handle();
            let worker_manager = WorkerManager::start(crate::settings::LegacyEncoding::Auto)
                .expect("the lazy worker manager should start");
            application
                .request_shutdown()
                .expect("the application shutdown message should be queued");
            application
                .run_application_without_tray(worker_manager)
                .expect("normal application shutdown should join the worker manager");
            // SAFETY: The normal application loop consumed and dropped its owned HWND.
            assert!(!unsafe { IsWindow(Some(application_handle)).as_bool() });

            let diagnostic =
                MessageWindow::create().expect("the diagnostic message window should be created");
            let diagnostic_handle = diagnostic.handle();
            let report = diagnostic
                .run_input_diagnostics(Duration::from_nanos(1))
                .expect("the minimum bounded diagnostic should finish");
            assert!(report.unmatched_changes() <= report.changed_samples());
            assert!(report.changed_samples() <= report.active_samples());

            // SAFETY: The consuming diagnostic loop has dropped its owned HWND.
            assert!(!unsafe { IsWindow(Some(diagnostic_handle)).as_bool() });
            assert!(
                registered_raw_devices()
                    .expect("the process registration should be queryable")
                    .is_empty(),
                "diagnostic timers and Raw Input should stop before window teardown"
            );

            for _ in 0..100 {
                let window = MessageWindow::create()
                    .expect("class cleanup should allow repeated message-window creation");
                let handle = window.handle();

                // SAFETY: `handle` belongs to the live window on this test thread.
                assert!(unsafe { IsWindow(Some(handle)).as_bool() });
                assert_raw_input_registrations(handle);
                drop(window);

                // SAFETY: Checking a stale HWND with IsWindow is the documented validity probe.
                assert!(!unsafe { IsWindow(Some(handle)).as_bool() });
                assert!(
                    registered_raw_devices()
                        .expect("the process registration should be queryable")
                        .is_empty(),
                    "raw input should be removed on every lifecycle"
                );
            }
        })
        .join()
        .expect("the message-window test thread should not panic");
    }

    #[test]
    fn timer_intervals_round_up_and_respect_windows_limits() {
        assert_eq!(timer_interval_ms(Duration::from_nanos(1)), 10);
        assert_eq!(timer_interval_ms(Duration::from_micros(10_001)), 11);
        assert_eq!(timer_interval_ms(Duration::from_millis(400)), 400);
        assert_eq!(timer_interval_ms(Duration::MAX), 2_147_483_647);
    }

    #[test]
    fn preview_practice_allows_more_operator_time_without_changing_evidence_timing() {
        assert_eq!(
            PREVIEW_WINDOW_DIAGNOSTIC_DURATION,
            Duration::from_millis(1_500)
        );
        assert_eq!(PREVIEW_WINDOW_PRACTICE_DURATION, Duration::from_secs(5));
    }

    #[test]
    fn preview_dismisses_on_relevant_or_unreadable_raw_input() {
        assert!(!preview_input_requires_dismissal(&Ok(Some(
            RawInputActivity::Mouse(RawMouseActivity::for_test(false, false))
        ))));
        assert!(preview_input_requires_dismissal(&Ok(Some(
            RawInputActivity::Mouse(RawMouseActivity::for_test(true, false))
        ))));
        assert!(preview_input_requires_dismissal(&Ok(Some(
            RawInputActivity::Mouse(RawMouseActivity::for_test(false, true))
        ))));
        assert!(preview_input_requires_dismissal(&Ok(Some(
            RawInputActivity::Keyboard
        ))));
        assert!(preview_input_requires_dismissal(&Ok(None)));
        assert!(preview_input_requires_dismissal(&Err(Error::empty())));
    }

    #[test]
    fn preview_guard_accepts_the_complete_target_rectangle_only_while_explorer_is_foreground() {
        let anchor = PhysicalScreenPoint::new(-400, 250);
        let bounds = PhysicalScreenRect::try_new(-400, 250, -350, 300).unwrap();
        assert!(preview_context_is_current(bounds, Some(anchor), true));
        assert!(preview_context_is_current(
            bounds,
            Some(PhysicalScreenPoint::new(-351, 299)),
            true
        ));
        assert!(!preview_context_is_current(
            bounds,
            Some(PhysicalScreenPoint::new(-350, 299)),
            true
        ));
        assert!(!preview_context_is_current(bounds, Some(anchor), false));
        assert!(!preview_context_is_current(bounds, None, true));
    }

    #[test]
    fn pointer_motion_preserves_only_the_active_target() {
        let bounds = PhysicalScreenRect::try_new(100, 200, 300, 400).unwrap();
        let active = Some(ActivePreview {
            generation: Generation::from_raw(24),
            anchor: PhysicalScreenPoint::new(150, 250),
            target_bounds: bounds,
        });

        assert!(pointer_motion_stays_on_active_target(
            active,
            PhysicalScreenPoint::new(299, 399)
        ));
        assert!(!pointer_motion_stays_on_active_target(
            active,
            PhysicalScreenPoint::new(300, 399)
        ));
        assert!(!pointer_motion_stays_on_active_target(
            None,
            PhysicalScreenPoint::new(150, 250)
        ));
    }

    #[test]
    fn stale_context_events_cannot_dismiss_a_newer_preview() {
        let generation = Generation::from_raw(25);
        let active = Some(ActivePreview {
            generation,
            anchor: PhysicalScreenPoint::new(300, 200),
            target_bounds: PhysicalScreenRect::try_new(250, 150, 350, 250).unwrap(),
        });

        assert!(!preview_context_generation_matches(active, 24));
        assert!(preview_context_generation_matches(active, 25));
        assert!(!preview_context_generation_matches(None, 25));
    }

    fn assert_raw_input_registrations(target: windows::Win32::Foundation::HWND) {
        let registrations =
            registered_raw_devices().expect("the process registrations should be queryable");
        assert_eq!(registrations.len(), 2);
        assert_eq!(
            registrations
                .iter()
                .map(|registration| registration.usUsage)
                .collect::<Vec<_>>(),
            vec![0x02, 0x06],
            "mouse and keyboard generic-desktop devices should be observed"
        );
        assert!(
            registrations.iter().all(|registration| {
                registration.hwndTarget == target && registration.dwFlags == RIDEV_INPUTSINK
            }),
            "both Raw Input classes should target the message window without capture flags"
        );
    }
}
