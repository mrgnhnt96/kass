#!/usr/bin/env python3
"""Put a trained shared adapter into the app, or check the ones it has.

    backend/venv/bin/python scripts/shared-adapters/publish.py cleanup 4B
    backend/venv/bin/python scripts/shared-adapters/publish.py voice
    backend/venv/bin/python scripts/shared-adapters/publish.py --check

Publishing copies build/shared-adapters/cleanup-<size>/ into
backend/assets/shared-adapters/ with a shared.json: its id, the cleanup
pipeline it was tested with, and its scores. It refuses an adapter that
doesn't beat the stock model on the test set (eval_cleanup.py's
eval-<size>-v1.json against eval-<size>-stock.json: fewer errors, and no
more changed facts or lost words), or that was scored with other cleanup
code.

`voice` copies build/shared-adapters/voice-turbo/ in float16, half the size,
once those exact weights pass Kass's own voice gate against plain turbo on
the public test speakers (score-voice.json) and on this Mac's own takes
(score-voice-mine.json), which never leave the Mac.

--check fails when a bundled cleanup adapter was tested with other cleanup
code, which the app would ignore; scripts/release.sh runs it. See
docs/plans/SHARED_ADAPTERS.md.
"""

import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
BUILD = ROOT / "build" / "shared-adapters"
ASSETS = ROOT / "backend" / "assets" / "shared-adapters"
FILES = ("adapters.safetensors", "adapter_config.json")


def check():
    from cleanup_pipeline import pipeline_id

    stale = []
    for manifest in sorted(ASSETS.glob("cleanup-*/shared.json")):
        if json.loads(manifest.read_text()).get("pipeline") != pipeline_id():
            stale.append(manifest.parent.name)
    if stale:
        sys.exit(
            f"Cleanup changed since these shared adapters were tested: {', '.join(stale)}. "
            "Retrain and publish them (docs/plans/SHARED_ADAPTERS.md), or Kass ignores them."
        )
    print("Shared adapters match this cleanup code.")


def publish_cleanup(size, evaluation, stock):
    from cleanup_pipeline import pipeline_id

    from backend.services.refinement import RefinementFlags

    source = BUILD / f"cleanup-{size}"
    result = json.loads(Path(evaluation).read_text())
    baseline = json.loads(Path(stock).read_text())
    if result["pipeline"] != pipeline_id() or baseline["pipeline"] != pipeline_id():
        sys.exit("The scores were made with other cleanup code. Score again with eval_cleanup.py.")
    if Path(result["adapter"]).resolve() != source.resolve() or baseline["adapter"]:
        sys.exit(f"Expected scores for {source} and for the stock {size} model.")
    after, before = result["summary"], baseline["summary"]
    if not (
        after["errors"] < before["errors"]
        and after["facts_changed"] <= before["facts_changed"]
        and after["words_dropped"] <= before["words_dropped"]
    ):
        sys.exit(
            f"Not better than stock: {after['errors']} errors, {after['facts_changed']} changed facts and "
            f"{after['words_dropped']} lost words; stock {before['errors']}, {before['facts_changed']} and "
            f"{before['words_dropped']}."
        )
    weights = (source / "adapters.safetensors").read_bytes()
    destination = ASSETS / f"cleanup-{size}"
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    for name in FILES:
        shutil.copy2(source / name, destination / name)
    manifest = {
        "id": f"cleanup-{size}-{hashlib.sha256(weights).hexdigest()[:12]}",
        "pipeline": pipeline_id(),
        # Trained and scored with the default cleanup prompt only.
        "tested_flags": [RefinementFlags().to_dict()],
        "scores": {"adapter": after, "stock": before},
        "training": json.loads((source / "training.json").read_text()),
    }
    (destination / "shared.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Published {manifest['id']}: {before['errors']} → {after['errors']} errors on the test set.")


def publish_voice(scores, mine):
    import mlx.core as mx

    from backend.services.voice_training import lora

    source = BUILD / "voice-turbo"
    weights = source / lora.ADAPTER_FILE
    result = json.loads(Path(scores).read_text())
    if result.get("adapter_sha256") != hashlib.sha256(weights.read_bytes()).hexdigest():
        sys.exit("These scores are for other weights. Score again with train_voice.py --score.")
    metrics = result["metrics"]
    if not metrics["passed"]:
        sys.exit(f"Not better than plain turbo: {'; '.join(metrics['reasons'])}.")
    # Public speech isn't dictation: one adapter that passed it did worse than
    # plain turbo on a real user's takes. Those takes must pass the same gate
    # against plain turbo (the personal voice model doesn't count here).
    if not Path(mine).is_file():
        sys.exit("Score it on this Mac's own takes too: train_voice.py --score --mine --out ...")
    own = json.loads(Path(mine).read_text())
    if own.get("adapter_sha256") != result["adapter_sha256"]:
        sys.exit("The scores on this Mac's takes are for other weights. Score again.")
    from backend.services.voice_training.gate import score_voice

    rows = [{key: value for key, value in row.items() if not key.startswith("production")} for row in own["rows"]]
    this_mac = score_voice(rows)
    if not this_mac["passed"]:
        sys.exit(f"Worse than plain turbo on this Mac's own takes: {'; '.join(this_mac['reasons'])}.")
    destination = ASSETS / "voice-turbo"
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    mx.save_safetensors(
        str(destination / lora.ADAPTER_FILE),
        {name: value.astype(mx.float16) for name, value in mx.load(str(weights)).items()},
    )
    config = json.loads((source / lora.CONFIG_FILE).read_text())
    config.pop("resumed_from", None)
    (destination / lora.CONFIG_FILE).write_text(json.dumps(config, indent=2) + "\n")
    shipped = (destination / lora.ADAPTER_FILE).read_bytes()
    summary = {key: metrics[key] for key in ("takes", "words", "base", "candidate")}
    manifest = {
        "id": f"voice-turbo-{hashlib.sha256(shipped).hexdigest()[:12]}",
        "scores": {"public": summary},
        "training": json.loads((source / "progress.json").read_text()),
    }
    manifest["scores"]["this_mac"] = {key: this_mac[key] for key in ("takes", "words", "base", "candidate")}
    (destination / "shared.json").write_text(json.dumps(manifest, indent=2) + "\n")
    before, after = metrics["base"], metrics["candidate"]
    print(
        f"Published {manifest['id']} ({len(shipped) / 1e6:.0f} MB): with talk and noise "
        f"{before['noisy']} → {after['noisy']} word errors, clean {before['clean']} → {after['clean']}."
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("kind", nargs="?", choices=("cleanup", "voice"))
    parser.add_argument("size", nargs="?", choices=("0.6B", "1.7B", "4B"))
    parser.add_argument("--eval", help="The adapter's scores (default eval-<size>-v1.json, or score-voice.json)")
    parser.add_argument("--stock", help="Stock scores (default build/shared-adapters/eval-<size>-stock.json)")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    if args.check:
        check()
    elif args.kind == "cleanup" and args.size:
        publish_cleanup(
            args.size,
            args.eval or BUILD / f"eval-{args.size}-v1.json",
            args.stock or BUILD / f"eval-{args.size}-stock.json",
        )
    elif args.kind == "voice":
        publish_voice(args.eval or BUILD / "score-voice.json", BUILD / "score-voice-mine.json")
    else:
        parser.error("name an adapter to publish, or --check")


if __name__ == "__main__":
    main()
