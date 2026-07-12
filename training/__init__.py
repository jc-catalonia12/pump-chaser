"""Historical candle ML pipeline for the MEXC trading bot (V2.0.0).

Offline only — live trading never trains. Pipeline:

  download → features → labels → dataset → walk_forward → export ONNX → registry
"""

__version__ = "2.0.0"
