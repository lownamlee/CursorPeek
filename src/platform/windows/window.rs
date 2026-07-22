use std::{
    cell::Cell,
    error::Error as StdError,
    fmt,
    marker::PhantomData,
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
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
use crate::worker::{
    CompletionNotifier, PendingWorkerPoll, PendingWorkerResolution, PreviewResult, WorkerManager,
    WorkerManagerError,
};

use super::{PreviewWindow, TrayCommand, TrayIcon};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, EVENT_SYSTEM_FOREGROUND,
                GetForegroundWindow, GetMessageW, HWND_MESSAGE, KillTimer, MSG, PostMessageW,
                RegisterClassW, SetTimer, TranslateMessage, USER_TIMER_MAXIMUM, USER_TIMER_MINIMUM,
                UnregisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WINEVENT_OUTOFCONTEXT,
                WINEVENT_SKIPOWNPROCESS, WM_APP, WM_INPUT, WM_TIMER, WNDCLASSW,
            },
        },
    },
    core::{Error, PCWSTR, Result, w},
};

#[cfg(test)]
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

pub(super) const CLASS_NAME: PCWSTR = w!("CursorPeek.MessageWindow");
const SHUTDOWN_MESSAGE: u32 = WM_APP + 1;
const WORKER_RESULT_MESSAGE: u32 = WM_APP + 2;
const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 3;
pub(super) const ACTIVATE_MESSAGE: u32 = WM_APP + 4;
const PREVIEW_CONTEXT_INVALIDATED_MESSAGE: u32 = WM_APP + 6;
const DWELL_TIMER_ID: usize = 1;
const INPUT_SAMPLE_TIMER_ID: usize = 2;
const INPUT_DIAGNOSTIC_DEADLINE_TIMER_ID: usize = 3;
const PREVIEW_DIAGNOSTIC_DEADLINE_TIMER_ID: usize = 4;
const PREVIEW_GUARD_TIMER_ID: usize = 5;
const PREVIEW_GUARD_INTERVAL: Duration = Duration::from_millis(125);

pub(crate) const PREVIEW_WINDOW_DIAGNOSTIC_DURATION: Duration = Duration::from_millis(1_500);
pub(crate) const PREVIEW_WINDOW_PRACTICE_DURATION: Duration = Duration::from_secs(5);

#[cfg(test)]
const TEST_PANIC_MESSAGE: u32 = WM_APP + 5;

pub(crate) struct MessageWindow {
    hwnd: HWND,
    dwell_timer: Option<WindowTimer>,
    preview_guard_timer: WindowTimer,
    hover_state: HoverState,
    preview_size: PreviewSize,
    input_diagnostics: Option<InputDiagnostics>,
    preview_diagnostics: Option<ActivePreviewDiagnostics>,
    preview_window: Option<PreviewWindow>,
    tray_icon: Option<TrayIcon>,
    paused: bool,
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
        let class = RegisteredWindowClass::register()?;

        // SAFETY: The class remains registered in `class`, all string pointers are static, the
        // module instance belongs to this process, and no creation parameter is passed. Using
        // HWND_MESSAGE creates a non-visible message-only window on the current thread.
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                CLASS_NAME,
                w!(""),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
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
            preview_window: None,
            tray_icon: None,
            paused: false,
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
    ) -> std::result::Result<(), ApplicationRunError> {
        self.worker_manager = Some(worker_manager);
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
            MessageLoopExit::InputDiagnostics(_) | MessageLoopExit::PreviewDiagnostics(_) => {
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
        self.record_preview_ui_task(ui_task_started.elapsed());

        match self.run_loop()? {
            MessageLoopExit::PreviewDiagnostics(report) => Ok(report),
            MessageLoopExit::Shutdown => {
                Ok(self.finish_preview_diagnostics(PreviewWindowDismissal::Shutdown))
            }
            MessageLoopExit::InputDiagnostics(_) => {
                unreachable!("the preview-window diagnostic cannot run input-coverage diagnostics")
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
                self.record_preview_ui_task(ui_task_started.elapsed());
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
                self.record_preview_ui_task(ui_task_started.elapsed());
                return Ok(MessageLoopExit::PreviewDiagnostics(
                    self.finish_preview_diagnostics(PreviewWindowDismissal::Timeout),
                ));
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == INPUT_SAMPLE_TIMER_ID
            {
                self.handle_input_diagnostic_sample();
                self.record_preview_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == DWELL_TIMER_ID
            {
                self.handle_dwell_timer(Instant::now());
                self.record_preview_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd
                && message.message == WM_TIMER
                && message.wParam.0 == PREVIEW_GUARD_TIMER_ID
            {
                self.guard_product_preview();
                self.record_preview_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == WORKER_RESULT_MESSAGE {
                self.handle_worker_result();
                self.record_preview_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == TRAY_CALLBACK_MESSAGE {
                if self.handle_tray_callback(message.wParam, message.lParam)? {
                    return Ok(MessageLoopExit::Shutdown);
                }
                self.record_preview_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == ACTIVATE_MESSAGE {
                if self.handle_instance_activation()? {
                    return Ok(MessageLoopExit::Shutdown);
                }
                self.record_preview_ui_task(ui_task_started.elapsed());
                continue;
            }

            if message.hwnd == self.hwnd && message.message == PREVIEW_CONTEXT_INVALIDATED_MESSAGE {
                self.handle_preview_context_invalidation(message.wParam.0);
                self.record_preview_ui_task(ui_task_started.elapsed());
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
                    self.record_preview_ui_task(ui_task_started.elapsed());
                    return Ok(MessageLoopExit::PreviewDiagnostics(
                        self.finish_preview_diagnostics(PreviewWindowDismissal::Input),
                    ));
                }
                self.handle_raw_input(raw_input);
            }
            self.record_preview_ui_task(ui_task_started.elapsed());
        }
    }

    fn record_preview_ui_task(&mut self, duration: Duration) {
        if let Some(diagnostics) = self.preview_diagnostics.as_mut() {
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
        if self.paused {
            return;
        }

        match raw_input {
            Ok(Some(RawInputActivity::Mouse(activity))) if activity.is_relevant() => {
                match physical_cursor_position() {
                    Ok(point) => self
                        .restart_dwell(PhysicalScreenPoint::new(point.x, point.y), Instant::now()),
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

                let Some(manager) = self.worker_manager.as_ref() else {
                    return;
                };
                let notifier = worker_result_notifier(self.hwnd);
                let Ok(pending) = manager.submit_with_notifier(generation, point, notifier) else {
                    return;
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
                        let generation = accept_worker_completion(
                            self.hover_state.generation(),
                            Some(resolution.generation()),
                        );
                        self.latest_worker_completion = generation;

                        if let (Some(generation), Some((anchor_generation, point))) =
                            (generation, anchor)
                            && generation == anchor_generation
                        {
                            self.show_worker_result(point, resolution.into_result());
                        } else {
                            self.hide_product_preview();
                        }
                    }
                    Err(_) => {
                        self.latest_worker_completion = None;
                        self.hide_product_preview();
                    }
                }
            }
        }
    }

    fn show_worker_result(&mut self, anchor: PhysicalScreenPoint, result: PreviewResult) {
        if matches!(result, PreviewResult::Status(_)) {
            self.hide_product_preview();
            return;
        }

        if self.preview_diagnostics.is_some() {
            return;
        }

        if self.preview_window.is_none() {
            let Ok(preview) = PreviewWindow::create() else {
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
                let active = ActivePreview { generation, anchor };
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
            active.anchor,
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
        let command = self
            .tray_icon
            .as_ref()
            .map(|tray| tray.command_for_callback(wparam, lparam, self.paused))
            .transpose()?
            .flatten();
        self.apply_tray_command(command)
    }

    fn handle_instance_activation(&mut self) -> Result<bool> {
        let command = self
            .tray_icon
            .as_ref()
            .map(|tray| tray.command_at_cursor(self.paused))
            .transpose()?
            .flatten();
        self.apply_tray_command(command)
    }

    fn apply_tray_command(&mut self, command: Option<TrayCommand>) -> Result<bool> {
        match command {
            None => Ok(false),
            Some(TrayCommand::TogglePaused) => {
                let paused = !self.paused;
                self.tray_icon
                    .as_mut()
                    .expect("a tray command requires the live tray owner")
                    .set_paused(paused)?;
                self.paused = paused;
                if paused {
                    self.cancel_dwell();
                }
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
    anchor: PhysicalScreenPoint,
    current: Option<PhysicalScreenPoint>,
    foreground_explorer: bool,
) -> bool {
    current == Some(anchor) && foreground_explorer
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

            // SAFETY: The target is the live message-only HWND owned by the registering thread.
            // The private message carries only a generation scalar, never a callback pointer or
            // borrowed event data.
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

    // SAFETY: These are the untouched parameters supplied by Windows to this window procedure.
    // Every WM_INPUT reaches this default procedure because the owning loop copies it before
    // dispatch and applies safe state only after required foreground cleanup.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::{
        ActivePreview, DWELL_TIMER_ID, IsWindow, LPARAM, MessageWindow,
        PREVIEW_WINDOW_DIAGNOSTIC_DURATION, PREVIEW_WINDOW_PRACTICE_DURATION, PostMessageW,
        TEST_PANIC_MESSAGE, WM_TIMER, WPARAM, accept_worker_completion, post_worker_result,
        preview_context_generation_matches, preview_context_is_current,
        preview_input_requires_dismissal, registered_raw_devices, timer_interval_ms,
    };
    use crate::hover::{Generation, PhysicalScreenPoint};
    use crate::platform::windows::explorer::is_explorer_window;
    use crate::platform::windows::input::{RawInputActivity, RawMouseActivity};
    use crate::worker::WorkerManager;
    use std::{
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::UI::Input::RIDEV_INPUTSINK;
    use windows::core::Error;

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
    fn message_window_lifecycle_and_callback_boundary_are_sound() {
        thread::spawn(|| {
            let mut first =
                MessageWindow::create().expect("the message-only window should be created");
            let first_handle = first.handle();

            // SAFETY: `first_handle` belongs to the live window on this test thread.
            assert!(unsafe { IsWindow(Some(first_handle)).as_bool() });
            assert!(
                !is_explorer_window(first_handle),
                "the private message window must fail the Explorer candidate gate"
            );
            assert_raw_input_registrations(first_handle);

            first.restart_dwell(PhysicalScreenPoint::new(-10, 20), Instant::now());
            assert!(first.dwell_timer_is_armed());
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
    fn preview_guard_requires_the_exact_anchor_and_foreground_explorer() {
        let anchor = PhysicalScreenPoint::new(-400, 250);
        assert!(preview_context_is_current(anchor, Some(anchor), true));
        assert!(!preview_context_is_current(
            anchor,
            Some(PhysicalScreenPoint::new(-399, 250)),
            true
        ));
        assert!(!preview_context_is_current(anchor, Some(anchor), false));
        assert!(!preview_context_is_current(anchor, None, true));
    }

    #[test]
    fn stale_context_events_cannot_dismiss_a_newer_preview() {
        let generation = Generation::from_raw(25);
        let active = Some(ActivePreview {
            generation,
            anchor: PhysicalScreenPoint::new(300, 200),
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
