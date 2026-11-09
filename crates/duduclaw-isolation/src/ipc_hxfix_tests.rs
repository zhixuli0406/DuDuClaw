//! HX security fix round (finding 8): `HelperWait`/`HelperStop`/`HelperCancel`
//! are employee-scoped like `HelperCall` [F7].
use super::*;

fn run3() -> RunHandle { RunHandle(crate::ids::RunId([3; 16])) }
fn call7() -> HelperCallId { HelperCallId::new([7; 16]).unwrap() }
fn employee() -> EmployeeId { EmployeeId([1; 16]) }
fn scoped() -> [SpawnRequest; 3] {
    [SpawnRequest::HelperWait { run: run3(), employee_id: employee(), call: call7() },
        SpawnRequest::HelperStop { run: run3(), employee_id: employee(), call: call7() },
        SpawnRequest::HelperCancel { run: run3(), employee_id: employee(), call: call7() }]
}

#[test]
fn hxfix_wait_stop_cancel_carry_and_require_employee_id() {
    for request in scoped() {
        let envelope = Envelope { version: 1, request_id: RequestId([2; 16]), message: request.clone() };
        assert_eq!(decode::<SpawnRequest>(&encode(&envelope).unwrap()).unwrap(), envelope);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["employee_id"], serde_json::to_value(employee()).unwrap());
        // The old (unscoped) shape no longer decodes: a gateway must name the employee.
        let mut old = value.clone();
        old.as_object_mut().unwrap().remove("employee_id");
        assert!(serde_json::from_value::<SpawnRequest>(old).is_err(), "{request:?}");
        // Still no caller-chosen slot, index or uid.
        for forbidden in ["slot", "index", "uid", "helper"] {
            let mut extra = value.clone();
            extra[forbidden] = 1.into();
            assert!(serde_json::from_value::<SpawnRequest>(extra).is_err(), "{forbidden}");
        }
        assert!(request.fd_plan().unwrap().is_empty());
    }
    // Reply pairing is unchanged by the new field.
    let [wait, stop, cancel] = scoped();
    let ended = SpawnResponse::HelperEnded { run: run3(), call: call7(), outcome: HelperOutcome::Exited(0) };
    assert_eq!(check_helper_reply(&wait, &ended, 0), Ok(()));
    for request in [stop, cancel] { assert_eq!(check_helper_reply(&request, &SpawnResponse::Ack, 0), Ok(())); }
}
