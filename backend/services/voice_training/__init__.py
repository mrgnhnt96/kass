"""Voice training: teach Whisper turbo to follow the user's voice in noise.

Kass keeps a bank of the user's clean takes
(bank.py) and a one-time download of other people talking and room noise
(the server downloads it; sounds.py reads it). While the Mac is idle, the model-improvement worker mixes the two
(mixing.py) and trains a small LoRA adapter on turbo (lora.py, train.py). The
manager activates a candidate only when it beats the current model with
background talk and noise and is no worse on clean takes; the speech backend
then merges it into turbo's weights at load, so recognition speed is
unchanged. See docs/plans/VOICE_TRAINING.md.
"""

from pathlib import Path

from ... import config


def root() -> Path:
    return config.get_data_dir() / "voice-training"
