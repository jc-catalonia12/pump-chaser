pub fn utc_now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}
