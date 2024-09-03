// use via_btc_client::inscriber::Inscriber;

use crate::resource::Resource;

/// Represents a client of a certain DA solution.
#[derive(Debug, Clone)]

// todo add trait to inscriber and then add it as resource in here
pub struct InscriberResource();

impl Resource for InscriberResource {
    fn name() -> String {
        "common/via_btc_inscriber".into()
    }
}
