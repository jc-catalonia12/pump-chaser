//! Lexicon-based crypto headline sentiment scorer (no external API).

/// Score a headline/body in [-1.0, 1.0].
pub fn score_text(text: &str) -> f64 {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    let mut score = 0.0f64;
    let mut hits = 0u32;
    for w in &words {
        if let Some(s) = word_score(w) {
            score += s;
            hits += 1;
        }
    }
    if hits == 0 {
        return 0.0;
    }
    (score / (hits as f64).sqrt()).clamp(-1.0, 1.0)
}

fn word_score(word: &str) -> Option<f64> {
    match word {
        "approval" | "approved" | "etf" | "bullish" | "surge" | "rally" => Some(1.0),
        "partnership" | "listing" | "listed" | "breakout" | "inflow" | "buyback" => Some(0.9),
        "launch" | "adoption" | "upgrade" | "institutional" | "accumulation" => Some(0.7),
        "record" => Some(0.6),
        "hack" | "hacked" | "rug" | "scam" | "ban" | "banned" | "bankrupt" | "bankruptcy" | "fraud" => {
            Some(-1.2)
        }
        "exploit" | "exploited" | "lawsuit" | "sued" | "delist" | "delisted" | "crash" | "plunge" => {
            Some(-1.0)
        }
        "dump" | "bearish" | "investigation" | "outflow" | "liquidation" | "liquidated" => Some(-0.8),
        "sec" => Some(-0.5),
        _ => None,
    }
}

/// Extract base tickers from text (e.g. BTC from "Bitcoin" or "BTC").
pub fn extract_symbols(text: &str) -> Vec<String> {
    let lower = format!(" {} ", text.to_lowercase());
    let mut found = Vec::new();
    for (alias, symbol) in SYMBOL_ALIASES {
        if lower.contains(alias) && !found.contains(&symbol.to_string()) {
            found.push(symbol.to_string());
        }
    }
    found
}

static SYMBOL_ALIASES: &[(&str, &str)] = &[
    (" bitcoin ", "BTC"),
    (" btc ", "BTC"),
    (" ethereum ", "ETH"),
    (" eth ", "ETH"),
    (" solana ", "SOL"),
    (" sol ", "SOL"),
    (" xrp ", "XRP"),
    (" ripple ", "XRP"),
    (" dogecoin ", "DOGE"),
    (" doge ", "DOGE"),
    (" cardano ", "ADA"),
    (" ada ", "ADA"),
    (" bnb ", "BNB"),
    (" avalanche ", "AVAX"),
    (" avax ", "AVAX"),
    (" polkadot ", "DOT"),
    (" dot ", "DOT"),
    (" chainlink ", "LINK"),
    (" link ", "LINK"),
    (" litecoin ", "LTC"),
    (" ltc ", "LTC"),
    (" pepe ", "PEPE"),
    (" shiba ", "SHIB"),
    (" shib ", "SHIB"),
    (" sui ", "SUI"),
    (" near ", "NEAR"),
    (" arbitrum ", "ARB"),
    (" optimism ", "OP"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hack_is_negative() {
        assert!(score_text("Major exchange hack drains funds") < -0.3);
    }

    #[test]
    fn etf_is_positive() {
        assert!(score_text("Bitcoin ETF approval expected") > 0.3);
    }

    #[test]
    fn extracts_btc() {
        let syms = extract_symbols("Bitcoin rallies after ETF news");
        assert!(syms.contains(&"BTC".to_string()));
    }
}
