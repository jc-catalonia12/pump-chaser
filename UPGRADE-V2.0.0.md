# Trading Bot Rebuild Roadmap

## Goal

Rebuild the trading bot into a modular machine-learning trading platform that can:

- Learn from historical market data
- Benchmark every model version
- Retrain automatically
- Deploy only models that outperform previous versions
- Keep the trading engine deterministic and testable

---

# Overall Architecture

```text
          MEXC Historical Data
                    │
                    ▼
           Feature Engineering
                    │
                    ▼
           Training Dataset Builder
                    │
                    ▼
      Walk-Forward Model Training
                    │
                    ▼
          ONNX Model Registry
                    │
        ┌───────────┴───────────┐
        ▼                       ▼
   Live Feature Engine     LLM Analysis
        │                       │
        └───────────┬───────────┘
                    ▼
           Trade Decision Engine
                    │
                    ▼
            Risk Management
                    │
                    ▼
               MEXC Futures
                    │
                    ▼
              Trade Database
                    │
                    ▼
          Periodic Retraining
```

---

# Phase 1 – Historical Data Collection

## Objective

Build a complete historical database.

### Tasks

- Download historical MEXC Futures candles
- Support:
  - 1m
  - 5m
  - 15m
  - 1h
  - 4h
- Store data locally

### Suggested Storage

```
data/
    raw/
        BTCUSDT_15m.parquet
        ETHUSDT_15m.parquet
```

Database fields

```
timestamp
symbol
open
high
low
close
volume
quote_volume
```

### Deliverables

- Historical downloader
- Automatic updates
- Local database

---

# Phase 2 – Feature Engineering

## Objective

Transform raw candles into machine-learning features.

### Indicators

- EMA20
- EMA50
- EMA100
- EMA200
- RSI
- MACD
- ATR
- ADX
- VWAP
- Bollinger Bands
- Volume MA
- Volatility

### Custom Features

- Candle body %
- Upper wick %
- Lower wick %
- Price return
- Rolling return
- Momentum
- Trend strength
- Time of day
- Day of week

### Deliverables

```
features/
    BTCUSDT_15m.parquet
```

---

# Phase 3 – Label Generation

## Objective

Automatically generate training labels.

Example

LONG

```
Future High >= Entry +1.5%

before

Future Low <= Entry -0.8%
```

SHORT

```
Future Low <= Entry -1.5%

before

Future High >= Entry +0.8%
```

Otherwise

```
NO TRADE
```

Target Classes

- LONG
- SHORT
- NO_TRADE

---

# Phase 4 – Dataset Builder

Combine

- Features
- Labels

Final dataset

```
timestamp
symbol

EMA20
EMA50
RSI
ATR
MACD
ADX
VWAP

...

Target
```

Save as

```
datasets/

training.parquet
```

---

# Phase 5 – Model Training

Train multiple models.

Candidates

- LightGBM
- XGBoost
- CatBoost
- Random Forest

Evaluate

- Precision
- Recall
- F1
- ROC AUC
- Brier Score

Select the best model.

Export

```
production.onnx
```

---

# Phase 6 – Walk Forward Validation

Never train and test on the same data.

Example

```
Train Jan-Mar

Test April

Retrain

Train Feb-Apr

Test May

Retrain

Train Mar-May

Test June
```

Deploy only if performance improves.

---

# Phase 7 – Model Registry

Maintain

```
models/

production.onnx

candidate.onnx

archive/
```

Each model should include

- Date
- Dataset version
- Metrics
- Notes

Never overwrite production without validation.

---

# Phase 8 – Live Inference

Live pipeline

```
Receive Candle

↓

Generate Indicators

↓

Generate Features

↓

ONNX Prediction

↓

Probability
```

No retraining during live trading.

---

# Phase 9 – LLM Analysis

The LLM should **not** directly decide trades.

Instead

Summarize market context.

Example

```
Trend

Bullish

Momentum

Strong

Volatility

Medium

Risk

Low
```

These outputs can be logged and optionally used as additional features.

---

# Phase 10 – Decision Engine

Example logic

```
Probability > 0.72

AND

ATR above threshold

AND

Volume above average

AND

Bull trend

↓

BUY
```

Rules should remain deterministic and easy to test.

---

# Phase 11 – Risk Management

Use Kelly sizing with safeguards.

Recommended

```
Kelly

↓

Half Kelly

↓

Maximum 5%

↓

Reduce during drawdown

↓

Reduce in high volatility
```

---

# Phase 12 – Trade Logger

Log every prediction.

Fields

```
timestamp

symbol

features

probability

prediction

confidence

regime

entry

exit

fees

slippage

PnL

MFE

MAE

reason
```

Store every decision, including those where no trade was taken.

---

# Phase 13 – Benchmark Dashboard

Model Metrics

- Accuracy
- Precision
- Recall
- F1
- ROC AUC
- Brier Score

Trading Metrics

- Net Profit
- Win Rate
- Profit Factor
- Sharpe Ratio
- Sortino Ratio
- Max Drawdown
- Average Win
- Average Loss

Operational Metrics

- Number of trades
- Fees
- Slippage
- Average hold time
- Regime distribution

---

# Phase 14 – Automatic Retraining

Weekly workflow

```
Download new candles

↓

Generate indicators

↓

Generate labels

↓

Train

↓

Walk-forward validation

↓

Compare with production

↓

If better

Deploy

Else

Keep current model
```

---

# Suggested Project Structure

```
trading-bot/

data/
    raw/
    features/
    labels/
    trades/

datasets/

indicators/

training/
    train.py
    validate.py
    walk_forward.py
    export_onnx.py

models/
    production.onnx
    candidate.onnx
    archive/

inference/

execution/

risk/

llm/

monitoring/

dashboard/

config/

tests/
```

---

# Long-Term Goals

## Version 1

- Historical downloader
- Feature engineering
- Basic ML model

---

## Version 2

- Multiple ML models
- Ensemble
- ONNX deployment

---

## Version 3

- Walk-forward retraining
- Model registry
- Automatic benchmarking

---

## Version 4

- Live monitoring dashboard
- Performance reports
- Automated retraining pipeline

---

## Version 5

- Multi-symbol portfolio
- Adaptive risk allocation
- Portfolio-level optimization
- Continuous model improvement