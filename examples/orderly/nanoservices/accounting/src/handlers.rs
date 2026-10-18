//! The handler trait implementation: one method per message this
//! nanoservice handles (routing.yaml). Handlers are the INTENT boundary:
//! they validate, write desired state through the typed store (`create`,
//! `update_spec`, `mark_deleted`, `nudge`) or the repository, dispatch
//! request-path adapters `Unfenced`, and send through `s` (the sender that
//! exposes exactly the declared sends). They never write status.

use basable_core::{AppError, Ctx};
use interfaces::{AccountingHandler, AccountingRoutes, AccountingSender};

use crate::Accounting;

// Messages stay qualified: a message may share its name with this
// component (an `order` nanoservice handling an `Order` response).
impl<R: AccountingRoutes> AccountingHandler<R> for Accounting {
    /// Books the snapshots sent: any mix of object types across processes,
    /// the objects of one process atomically or not at all, one outcome per
    /// snapshot in the order sent. Rendered whole; the rules it books by are
    /// in `src/ledger/`.
    async fn handle_book(&self, _ctx: &Ctx, msg: messages::Book, _s: AccountingSender<'_, R>) -> Result<messages::Booked, AppError> {
        let mut request = basable_ledger::Request::new();
        for snapshot in msg.snapshots {
            match snapshot {
                messages::LedgerSnapshot::Order(o) => {
                    request.add(o);
                }
                messages::LedgerSnapshot::Payment(o) => {
                    request.add(o);
                }
            }
        }
        let outcomes = self
            .ledger
            .book(request)
            .await
            .map_err(|e| AppError::internal(format!("ledger: {e}")))?;
        Ok(messages::Booked {
            outcomes: outcomes.into_iter().map(Into::into).collect(),
        })
    }
}
