//! Old versions saved a one-time setup code in `<data_dir>/setup_code.txt`. The code is gone:
//! the first account made on a fresh server is the owner. This removes the old file.
use crate::config::Config;

pub fn remove_file(cfg: &Config) {
    let _ = std::fs::remove_file(cfg.data_dir.join("setup_code.txt"));
}
