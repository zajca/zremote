use zremote_client::ApiClient;

use crate::format::Formatter;

/// Show server status: mode, version, and connected host count.
pub async fn run(client: &ApiClient, fmt: &dyn Formatter) -> i32 {
    let mode_info = match client.get_mode_info().await {
        Ok(info) => info,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };

    let hosts = match client.list_hosts().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("Error fetching hosts: {e}");
            return 1;
        }
    };

    println!("{}", fmt.status_info(&mode_info, &hosts));
    0
}
