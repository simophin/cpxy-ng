use crate::encrypt_stream::Configuration;
use std::num::NonZeroUsize;

pub fn select_cipher_based_on_port(port: u16) -> (Configuration, Configuration) {
    match port {
        443 | 465 | 993 | 5223 => (
            Configuration::random_partial(NonZeroUsize::new(32).unwrap()),
            Configuration::random_partial(NonZeroUsize::new(512).unwrap()),
        ),
        _ => (Configuration::random_full(), Configuration::random_full()),
    }
}
