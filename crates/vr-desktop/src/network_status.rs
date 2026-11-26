//! Outbound internet status is independent of the routes offered for inbound sharing.
#[cfg_attr(not(any(windows, test)), allow(dead_code))] // OS-specific states come from Windows NCSI.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Internet {
    Unknown,
    Online,
    Local,
    Offline,
    Limited,
}
impl Internet {
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "互联网 · 未验证",
            Self::Online => "互联网 · 已连接",
            Self::Local => "系统报告 · 本地网络",
            Self::Offline => "系统报告 · 未联网",
            Self::Limited => "系统报告 · 访问受限",
        }
    }
}
/// Uses the same OS connectivity assessment as Windows' network indicator. No third-party probe.
#[cfg(windows)]
pub fn system_hint() -> Internet {
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::GetNetworkConnectivityHint, Networking::WinSock::*,
    };
    let mut h = NL_NETWORK_CONNECTIVITY_HINT::default();
    if unsafe { GetNetworkConnectivityHint(&mut h) } != 0 {
        return Internet::Unknown;
    }
    classify(h.ConnectivityLevel)
}
#[cfg(any(windows, test))]
fn classify(level: i32) -> Internet {
    match level {
        1 => Internet::Offline,
        2 => Internet::Local,
        3 => Internet::Online,
        4 => Internet::Limited,
        _ => Internet::Unknown,
    }
}
/// A bounded outbound-only check, started automatically and refreshed during sharing.
/// Do not follow a captive portal redirect or infer that the agent's port is open.
pub async fn check_https() -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    else {
        return false;
    };
    client
        .head("https://www.qq.com/")
        .send()
        .await
        .is_ok_and(|r| r.status().is_success() || r.status().is_redirection())
}
#[cfg(not(windows))]
pub fn system_hint() -> Internet {
    Internet::Unknown
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outbound_online_is_not_an_ingress_error() {
        assert_eq!(classify(3), Internet::Online);
        assert_eq!(classify(2), Internet::Local);
        assert_eq!(classify(1), Internet::Offline);
        assert_eq!(classify(0), Internet::Unknown);
    }
}
