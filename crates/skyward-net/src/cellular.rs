//! SIM readiness and packet-domain registration from the modem, without
//! querying IMSI, ICCID, phone numbers or authentication material.
use crate::{qmi, qrtr};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Serialize)]
pub struct Status {
    pub read_only: bool,
    pub endpoints: Vec<qrtr::Endpoint>,
    pub cards: Vec<Answer<Vec<qmi::Card>>>,
    pub registration: Vec<Answer<qmi::ServingSystem>>,
    pub operating_modes: Vec<Answer<qmi::OperatingMode>>,
    pub data_formats: Vec<Answer<qmi::DataFormat>>,
    pub signals: Vec<Answer<Vec<qmi::Signal>>>,
}
#[derive(Serialize)]
pub struct Answer<T> {
    pub endpoint: qrtr::Endpoint,
    pub value: Option<T>,
    pub error: Option<String>,
}
fn answer<T>(endpoint: qrtr::Endpoint, result: Result<T>) -> Answer<T> {
    match result {
        Ok(value) => Answer { endpoint, value: Some(value), error: None },
        Err(error) => Answer { endpoint, value: None, error: Some(format!("{error:#}")) },
    }
}
pub fn status() -> Result<Status> {
    let endpoints = qrtr::discover()?;
    let relevant: Vec<_> = endpoints.iter().copied().filter(|e| matches!(e.service,2|3|11|26)).collect();
    ensure!(relevant.len() <= 16, "unexpected number of cellular status services");
    let mut cards = Vec::new(); let mut registration = Vec::new();
    let mut operating_modes = Vec::new(); let mut data_formats = Vec::new(); let mut signals = Vec::new();
    for endpoint in relevant {
        match endpoint.service {
            11 => cards.push(answer(endpoint,qrtr::status(endpoint,qmi::StatusRequest::Cards,qmi::cards))),
            3 => {
                registration.push(answer(endpoint,qrtr::status(endpoint,qmi::StatusRequest::ServingSystem,qmi::serving_system)));
                signals.push(answer(endpoint,qrtr::status(endpoint,qmi::StatusRequest::Signal,qmi::signal)));
            },
            2 => operating_modes.push(answer(endpoint,qrtr::status(endpoint,qmi::StatusRequest::OperatingMode,qmi::operating_mode))),
            26 => data_formats.push(answer(endpoint,qrtr::status(endpoint,qmi::StatusRequest::DataFormat,qmi::data_format))),
            _ => unreachable!(),
        }
    }
    let endpoints = endpoints.into_iter().filter(|e| matches!(e.service, 1|2|3|11|14|26|47|49|69)).collect();
    Ok(Status { read_only: true, endpoints, cards, registration, operating_modes, data_formats, signals })
}
pub fn status_json() -> Result<String> { Ok(serde_json::to_string_pretty(&status()?)?) }

/// Only the primary modem's current public readings. WDA transport setup
/// is deliberately excluded from SIM health and from this read-only UI.
pub fn mobile() -> Result<crate::mobile::Mobile> {
    // A failure of one service must leave the successful public readings
    // intact, while retaining the reason instead of pretending there is no SIM.
    fn read<T>(result: Result<T>, label: &str, issues: &mut Vec<String>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => { issues.push(format!("{label}: {error:#}")); None }
        }
    }
    let endpoints = qrtr::discover()?;
    let primary: Vec<_> = endpoints.into_iter().filter(|e| e.node == 0 && e.instance == 0 && matches!(e.service,2|3|11)).collect();
    let mut issues = Vec::new();
    for (service,label) in [(2,"radio power"),(3,"network registration"),(11,"SIM cards")] {
        let count = primary.iter().filter(|e| e.service == service).count();
        ensure!(count <= 1,"ambiguous primary modem status endpoint for {label}");
        if count == 0 { issues.push(format!("{label}: modem service unavailable")); }
    }
    let mut mobile = crate::mobile::Mobile::default();
    for endpoint in primary {
        match endpoint.service {
            11 => mobile.cards = read(qrtr::status(endpoint,qmi::StatusRequest::Cards,qmi::cards),"SIM cards",&mut issues),
            2 => mobile.power = read(qrtr::status(endpoint,qmi::StatusRequest::OperatingMode,qmi::operating_mode),"radio power",&mut issues),
            3 => {
                mobile.network = read(qrtr::status(endpoint,qmi::StatusRequest::ServingSystem,qmi::serving_system),"network registration",&mut issues);
                mobile.signals = read(qrtr::status(endpoint,qmi::StatusRequest::Signal,qmi::signal),"signal strength",&mut issues).unwrap_or_default();
                if let Some(current) = mobile.network.as_mut().and_then(|n| n.plmn.as_mut()).filter(|p| p.description.is_empty())
                    && let Some(home) = read(qrtr::status(endpoint,qmi::StatusRequest::HomeNetwork,qmi::home_network),"operator name",&mut issues)
                {
                    // A home operator is not the serving operator while
                    // roaming. Fill only the very same reported PLMN.
                    if (current.mcc,current.mnc) == (home.mcc,home.mnc) { current.description=home.description; }
                }
            },
            _ => unreachable!(),
        }
    }
    mobile.available = mobile.cards.is_some() || mobile.power.is_some() || mobile.network.is_some();
    mobile.incomplete = mobile.cards.is_none() || mobile.power.is_none() || mobile.network.is_none();
    if !issues.is_empty() { mobile.issue = Some(issues.join("; ")); }
    Ok(mobile)
}

/// Explicit radio power-on, separate from all read-only status queries.
/// No carrier profile, SIM preference or persistent autoconnect is set.
pub fn radio_online() -> Result<()> {
    let endpoints = qrtr::discover()?;
    let modes: Vec<_> = endpoints.into_iter().filter(|e| e.service == 2 && e.instance == 0 && e.node == 0).collect();
    ensure!(modes.len() == 1, "expected one primary DMS radio service");
    let client = std::cell::RefCell::new(qrtr::Client::new(modes[0])?);
    let until = Instant::now() + Duration::from_secs(45);
    online_with(
        |timeout| client.borrow_mut().request(qmi::StatusRequest::OperatingMode.message(), &[], timeout, qmi::operating_mode),
        |timeout| client.borrow_mut().request(0x2e, &[(1, &[0])], timeout, |_| Ok(())),
        || until.saturating_duration_since(Instant::now()),
        std::thread::sleep,
    )
}

/// Retry only the explicit startup responses DeviceNotReady (52) and
/// InformationUnavailable (74),
/// as defined by https://github.com/linux-mobile-broadband/libqmi/blob/main/src/libqmi-glib/qmi-errors.h.
/// Every retry first reads the real mode. A successful Set acknowledges an
/// asynchronous transition: after it, only Get is ever sent.
fn online_with(
    mut get: impl FnMut(Duration) -> Result<qmi::OperatingMode>,
    mut set: impl FnMut(Duration) -> Result<()>,
    remaining: impl Fn() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<()> {
    let retryable = |error: &anyhow::Error| error.downcast_ref::<qmi::ProtocolError>()
        .is_some_and(|e| e.startup_unavailable());
    let mut acknowledged = false;
    let mut last = "no DMS operating mode read".to_string();
    loop {
        let left = remaining();
        ensure!(!left.is_zero(), "radio startup deadline expired: {last}");
        let mode = match get(left.min(Duration::from_secs(3))) {
            Ok(mode) => mode,
            Err(error) if retryable(&error) => {
                last = format!("DMS Get Operating Mode: {error:#}");
                sleep(remaining().min(Duration::from_secs(1)));
                continue;
            }
            Err(error) => return Err(error).context("DMS Get Operating Mode during startup"),
        };
        ensure!(mode.hardware_restricted != Some(true), "radio is hardware restricted (DMS mode {})", mode.mode);
        if mode.mode == 0 { return Ok(()); }
        ensure!(matches!(mode.mode, 1|3|5|6|7), "refuse radio transition from DMS mode {}", mode.mode);
        last = format!("actual DMS mode {}, Set Online acknowledged={acknowledged}", mode.mode);
        if !acknowledged {
            let left = remaining();
            ensure!(!left.is_zero(), "radio startup deadline expired: {last}");
            match set(left.min(Duration::from_secs(15))) {
                Ok(()) => { acknowledged = true; last = format!("DMS Set Online acknowledged; last actual mode {}", mode.mode); }
                Err(error) if retryable(&error) => { last = format!("DMS Set Online from mode {}: {error:#}", mode.mode); }
                Err(error) => return Err(error).with_context(|| format!("DMS Set Online from mode {}", mode.mode)),
            }
        }
        sleep(remaining().min(Duration::from_secs(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    enum Step { Get(Result<qmi::OperatingMode>), Set(Result<()>) }
    fn mode(mode: u8) -> Step { Step::Get(Ok(qmi::OperatingMode { mode, hardware_restricted: Some(false) })) }
    fn protocol(code: u16) -> anyhow::Error { qmi::ProtocolError { status: 1, code }.into() }

    // The script checks the actual operation sequence, including every
    // forbidden repeat Set. A virtual clock exercises deadlines instantly.
    fn run(steps: Vec<Step>, seconds: u64) -> Result<()> {
        let steps = RefCell::new(VecDeque::from(steps));
        let left = Cell::new(Duration::from_secs(seconds));
        let result = online_with(
            |timeout| {
                assert!(timeout <= Duration::from_secs(3) && timeout <= left.get());
                match steps.borrow_mut().pop_front().expect("unexpected Get") {
                    Step::Get(value) => value, Step::Set(_) => panic!("Get expected before a Set retry"),
                }
            },
            |timeout| {
                assert!(timeout <= Duration::from_secs(15) && timeout <= left.get());
                match steps.borrow_mut().pop_front().expect("unexpected Set") {
                    Step::Set(value) => value, Step::Get(_) => panic!("an acknowledged Set must not repeat"),
                }
            },
            || left.get(),
            |wait| { assert!(wait <= Duration::from_secs(1)); left.set(left.get().saturating_sub(wait)); },
        );
        assert!(steps.borrow().is_empty(), "expected operations were skipped");
        result
    }

    #[test]
    fn startup_unavailability_retries_only_until_set_is_acknowledged() {
        for code in [52,74] {
            run(vec![Step::Get(Err(protocol(code))), mode(5), Step::Set(Err(protocol(code))),
                mode(5), Step::Set(Ok(())), mode(5), Step::Get(Err(protocol(code))), mode(0)], 45).unwrap();
            // A failed Set may nevertheless be followed by an already-online
            // reading. Never send a redundant control request in that case.
            run(vec![mode(5), Step::Set(Err(protocol(code))), mode(0)], 45).unwrap();
        }
        run(vec![mode(5),Step::Set(Err(protocol(52))),Step::Get(Err(protocol(74))),
            mode(5),Step::Set(Err(protocol(74))),mode(0)],45).unwrap();
    }

    #[test]
    fn hardware_restriction_and_unsupported_modes_never_become_success() {
        for current in [0,1,5] {
            let error = run(vec![Step::Get(Ok(qmi::OperatingMode { mode:current,hardware_restricted:Some(true) }))],45).unwrap_err();
            assert!(error.to_string().contains("hardware restricted"));
        }
        for current in [2,4,8,255] { assert!(run(vec![mode(current)],45).is_err()); }
        assert!(run(vec![mode(5),Step::Set(Ok(())),mode(2)],45).is_err());
        assert!(run(vec![mode(5),Step::Set(Ok(())),
            Step::Get(Ok(qmi::OperatingMode { mode:0,hardware_restricted:Some(true) }))],45).is_err());
    }

    #[test]
    fn other_protocol_and_transport_errors_do_not_retry() {
        for code in [26,53,60,83,118] {
            assert!(run(vec![Step::Get(Err(protocol(code)))],45).unwrap_err().to_string().contains("Get Operating Mode"));
            assert!(run(vec![mode(5),Step::Set(Err(protocol(code)))],45).unwrap_err().to_string().contains("Set Online"));
        }
        for code in [52,74] {
            assert!(run(vec![Step::Get(Err(anyhow::anyhow!("transport error={code}")))],45).is_err());
            for status in [0,2] {
                assert!(run(vec![Step::Get(Err(qmi::ProtocolError{status,code}.into()))],45).is_err());
                assert!(run(vec![mode(5),Step::Set(Err(qmi::ProtocolError{status,code}.into()))],45).is_err());
            }
        }
        assert!(run(vec![mode(5),Step::Set(Err(anyhow::anyhow!("request timed out")))],45).is_err());
    }

    #[test]
    fn unavailable_or_acknowledged_but_offline_transition_has_a_deadline() {
        for (code,name) in [(52,"DeviceNotReady"),(74,"InformationUnavailable")] {
            let error = run((0..3).map(|_| Step::Get(Err(protocol(code)))).collect(),3).unwrap_err();
            assert!(error.to_string().contains("deadline") && error.to_string().contains(name));
            let steps = (0..3).flat_map(|_| [mode(5),Step::Set(Err(protocol(code)))]).collect();
            let error = run(steps,3).unwrap_err();
            assert!(error.to_string().contains("deadline") && error.to_string().contains(name));
        }
        let error = run(vec![mode(5),Step::Set(Ok(())),mode(5),mode(5)],3).unwrap_err();
        assert!(error.to_string().contains("deadline") && error.to_string().contains("mode 5"));
    }
}
