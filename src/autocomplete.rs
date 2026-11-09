//! The autocompleter of contract §19: async client only. A blocking call cannot be cancelled in flight, and
//! autocomplete is a UI behaviour whose UIs use the async client.

use std::fmt;
use std::future::Future;
use std::sync::{Mutex, PoisonError};

use futures_util::future::{AbortHandle, Abortable};

use crate::client::AtgeoClient;
use crate::error::Error;
use crate::text::qualifies;
use crate::transport::HttpTransport;
use crate::types::{Disclosure, KeywordOptions, KeywordSearchResult};

/// An autocompleter for one text field, from
/// [`AtgeoClient::autocomplete_by_keyword`](crate::AtgeoClient::autocomplete_by_keyword).
///
/// Each text field makes its own: "the previous call" belongs to the autocompleter, so two fields on one page,
/// sharing a client, do not supersede each other. It holds only its options and the previous call's abort
/// handle, and caches nothing.
pub struct KeywordAutocomplete<'a, T> {
    client: &'a AtgeoClient<T>,
    options: KeywordOptions,
    previous: Mutex<Option<AbortHandle>>,
}

impl<T> fmt::Debug for KeywordAutocomplete<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeywordAutocomplete").field("options", &self.options).finish_non_exhaustive()
    }
}

impl<'a, T: HttpTransport> KeywordAutocomplete<'a, T> {
    /// Validates what the options carry, as `search_by_keyword` does, before any call is made.
    pub(crate) fn new(client: &'a AtgeoClient<T>, options: KeywordOptions) -> Result<Self, Error> {
        if let Some(region) = &options.region {
            crate::region::validate(&region.as_region())?;
        }
        Ok(KeywordAutocomplete { client, options, previous: Mutex::new(None) })
    }

    /// Searches for places whose names the keyword matches, superseding the previous call.
    ///
    /// **Every keyword that qualifies is sent to the server as given, prefixes included**: typing `Starbucks`
    /// sends `St`, `Sta`, `Star` and so on, each as a request of its own, unless the app sends fewer. The
    /// server keeps no log of it (garganorn's privacy design); a hostile operator can record what it
    /// receives. Debouncing reduces how many prefixes go out and is the app's choice: this does not
    /// debounce. A keyword the server would refuse (no word of two characters among its first twelve
    /// distinct words) makes no request and resolves with an empty result whose `disclosed` is
    /// `keyword: false`; one the server refuses anyway, with `KeywordRequired`, resolves empty with
    /// `keyword: true`, since it went out.
    ///
    /// The previous call is superseded when this method is called, not when the returned future is first
    /// polled, and it then resolves to [`Error::Superseded`]; its request, if it had one in flight, is
    /// dropped, and one that had not started is never sent. Ignore `Error::Superseded`: it is not the
    /// caller's doing. Dropping a returned future cancels its call, and the caller's own cancellation is
    /// always that.
    ///
    /// Other failures are those of [`search_by_keyword`](AtgeoClient::search_by_keyword).
    pub fn search_by_keyword(
        &self,
        keyword: &str,
    ) -> impl Future<Output = Result<KeywordSearchResult, Error>> + Send + use<'a, T> {
        let call = if qualifies(keyword) {
            let (handle, registration) = AbortHandle::new_pair();
            let keyword = keyword.to_owned();
            let (client, options) = (self.client, self.options.clone());
            let sent_region = options.region.is_some();
            let work = async move {
                match client.search_by_keyword(&keyword, options).await {
                    // The server refused a keyword the qualifying check let through: nothing to show, but the
                    // keyword went out.
                    Err(Error::Atgeo(e)) if e.code == "KeywordRequired" => {
                        Ok(KeywordSearchResult::empty(Disclosure::new(true, sent_region.then_some(1.0))))
                    }
                    other => other,
                }
            };
            Some((Abortable::new(work, registration), handle))
        } else {
            None
        };
        // Superseding happens now, in call order, whatever order the futures are polled in.
        let (work, handle) = match call {
            Some((work, handle)) => (Some(work), Some(handle)),
            None => (None, None),
        };
        let replaced = std::mem::replace(&mut *self.previous.lock().unwrap_or_else(PoisonError::into_inner), handle);
        if let Some(previous) = replaced {
            previous.abort();
        }
        async move {
            match work {
                Some(work) => work.await.unwrap_or(Err(Error::Superseded)),
                None => Ok(KeywordSearchResult::empty(Disclosure::new(false, None))),
            }
        }
    }
}
