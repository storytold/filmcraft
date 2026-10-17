//! A reset either flushes the current native session or requires a fresh one before input.

pub(super) fn reset_session<T>(
    session: &mut Option<T>,
    ensure_com: impl FnOnce() -> Result<(), String>,
    flush: impl FnOnce(&T) -> Result<(), String>,
) -> Result<(), String> {
    let result = ensure_com().and_then(|_| session.as_ref().map(flush).transpose().map(|_| ()));
    if result.is_err() {
        // Clearing our metadata alone does not discard the native references. Recreation does.
        *session = None;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mf_reset_com_or_flush_failure_discards_session_before_reopening_run() {
        let mut session = Some(1);
        assert!(reset_session(&mut session, || Err("COM unavailable".into()), |_| panic!("COM failure must not flush")).is_err());
        assert!(session.is_none(), "failed COM cannot leave an unflushed session reusable");
        session = Some(2);
        assert!(reset_session(&mut session, || Ok(()), |_| Err("native flush failed".into())).is_err());
        assert!(session.is_none());
        session = Some(3);
        assert!(
            reset_session(
                &mut session,
                || Ok(()),
                |value| {
                    assert_eq!(*value, 3);
                    Ok(())
                }
            )
            .is_ok()
        );
        assert_eq!(session, Some(3));
    }
}
