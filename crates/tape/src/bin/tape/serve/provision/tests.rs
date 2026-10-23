use super::{classify_ha_create_result, ProvisionDecision};
use tape_journal::application::JournalError;
use tape_shared_kernel::{Subscription, SubscriptionError};

#[test]
fn subscription_created_ends_attempt_as_success() {
    let sub = Subscription {
        topic: "orders".into(),
        name: "worker-1".into(),
    };
    assert_eq!(
        classify_ha_create_result(&Ok(sub)),
        ProvisionDecision::Created
    );
}

#[test]
fn already_exists_ends_attempt_as_success() {
    let err = JournalError::from(SubscriptionError::AlreadyExists {
        topic: "orders".into(),
        name: "worker-1".into(),
    });
    assert_eq!(
        classify_ha_create_result(&Err(err)),
        ProvisionDecision::AlreadyExists
    );
}

#[test]
fn unavailable_proposal_is_retried() {
    let err = JournalError::Unavailable("raft group has no leader".into());
    assert_eq!(
        classify_ha_create_result(&Err(err)),
        ProvisionDecision::Retry("raft group has no leader".into())
    );
}

#[test]
fn fatal_errors_end_attempt_without_retry() {
    let err = JournalError::StorageDegraded;
    assert!(matches!(
        classify_ha_create_result(&Err(err)),
        ProvisionDecision::Failed(_)
    ));

    let err2 = JournalError::Internal("poisoned".into());
    assert!(matches!(
        classify_ha_create_result(&Err(err2)),
        ProvisionDecision::Failed(_)
    ));

    let err3 = JournalError::Conflict("conflict".into());
    assert!(matches!(
        classify_ha_create_result(&Err(err3)),
        ProvisionDecision::Failed(_)
    ));
}
