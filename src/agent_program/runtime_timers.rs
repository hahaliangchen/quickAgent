//! Durable turn-local callbacks. The host waits; no OS timer/thread is serialized.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Callback {
    function: VmValue,
    args: Vec<VmValue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Timer {
    callback: Callback,
    due_at_ms: i64,
    period_ms: Option<i64>,
}

pub(super) fn is_global(name: &str) -> bool {
    matches!(
        name,
        "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval" | "queueMicrotask"
    )
}

impl Runtime {
    fn timer_now(&self) -> i64 {
        #[cfg(test)]
        if let Some(now) = self.timer_test_time {
            return now;
        }
        chrono::Utc::now().timestamp_millis()
    }

    #[cfg(test)]
    pub(crate) fn set_test_timer_time(&mut self, now: i64) {
        self.timer_test_time = Some(now);
    }

    /// Absolute wall-clock wake time; persists unchanged across pause/restart.
    pub fn next_timer_at(&self) -> Option<i64> {
        if self.finished {
            return None;
        }
        self.timers.values().map(|timer| timer.due_at_ms).min()
    }

    pub fn timer_wait(&self) -> Option<std::time::Duration> {
        self.next_timer_at().map(|due| {
            std::time::Duration::from_millis(due.saturating_sub(self.timer_now()).max(0) as u64)
        })
    }

    /// Cancellation is terminal, unlike a user interjection's resumable pause.
    pub fn cancel(&mut self) {
        self.fail_capacity(json!({"code":"PROGRAM_CANCELLED","message":"program cancelled; scheduled callbacks were cleared"}));
    }

    pub(super) fn clear_scheduled_callbacks(&mut self) {
        self.timers.clear();
        self.microtasks.clear();
    }

    pub(super) fn timer_builtin(&mut self, name: &str, args: Vec<VmValue>) -> Result<VmValue> {
        if matches!(name, "clearTimeout" | "clearInterval") {
            // Numeric IDs (or their string form); unrelated handles are no-ops.
            if let Some(value @ (VmValue::Number(_) | VmValue::String(_))) = args.first() {
                let number = super::super::js_value::to_number(&self.heap, value)?;
                if number.is_finite() && number >= 1.0 && number.fract() == 0.0 {
                    self.timers.remove(&(number as u64));
                }
            }
            return Ok(VmValue::Undefined);
        }
        let function = args.first().cloned().unwrap_or(VmValue::Undefined);
        ensure!(
            super::super::vm_eval::callable(&self.heap, &function),
            "TypeError: {name} callback must be a function"
        );
        if name == "queueMicrotask" {
            let id = super::super::promise::ContinuationId(self.next_continuation);
            self.next_continuation = self
                .next_continuation
                .checked_add(1)
                .context("continuation id exhausted")?;
            self.microtasks.insert(
                id,
                Callback {
                    function,
                    args: vec![],
                },
            );
            self.promises.enqueue(
                id,
                super::super::promise::Outcome::Fulfilled(VmValue::Undefined),
            );
            return Ok(VmValue::Undefined);
        }
        let delay = super::super::js_value::to_number(
            &self.heap,
            args.get(1).unwrap_or(&VmValue::Undefined),
        )?;
        let delay = if !delay.is_finite() || !(1.0..=2_147_483_647.0).contains(&delay) {
            1
        } else {
            delay.trunc() as i64
        };
        let id = self
            .next_timer_id
            .checked_add(1)
            .filter(|id| *id <= 9_007_199_254_740_991)
            .context("timer id exhausted")?;
        self.next_timer_id = id;
        self.timers.insert(
            id,
            Timer {
                callback: Callback {
                    function,
                    args: args.into_iter().skip(2).collect(),
                },
                due_at_ms: self.timer_now().saturating_add(delay),
                period_ms: (name == "setInterval").then_some(delay),
            },
        );
        Ok(VmValue::Number(JsNumber::new(id as f64)))
    }

    pub(super) fn start_callback(&mut self, callback: Callback) -> Result<()> {
        // A real wrapper frame makes native/tool callbacks suspend exactly like
        // ordinary calls, with no synthetic Promise to swallow uncaught throws.
        let args = self.heap.allocate(HeapNode::Array(callback.args))?;
        let mut frame = Self::vm_frame(
            &self.program,
            &mut self.heap,
            self.module_env,
            "$$runCallback",
            vec![callback.function, VmValue::Reference(args)],
            None,
        )?;
        frame.task_callback = true;
        self.frames.push(frame);
        Ok(())
    }

    pub(super) fn dispatch_due_timer(&mut self) -> Result<bool> {
        let now = self.timer_now();
        let Some(id) = self
            .timers
            .iter()
            .filter(|(_, timer)| timer.due_at_ms <= now)
            .min_by_key(|(id, timer)| (timer.due_at_ms, **id))
            .map(|(id, _)| *id)
        else {
            return Ok(false);
        };
        let mut timer = self.timers.remove(&id).unwrap();
        let callback = timer.callback.clone();
        if let Some(period) = timer.period_ms {
            // Never replay missed ticks, including after a host restart.
            timer.due_at_ms = now.saturating_add(period);
            self.timers.insert(id, timer);
        }
        self.refresh_instruction_segment();
        self.start_callback(callback)?;
        Ok(true)
    }

    pub(super) fn callback_roots(&self, roots: &mut Vec<VmValue>) {
        for callback in self
            .timers
            .values()
            .map(|timer| &timer.callback)
            .chain(self.microtasks.values())
        {
            roots.push(callback.function.clone());
            roots.extend(callback.args.iter().cloned());
        }
    }
}
