#!/usr/bin/env python3
from __future__ import annotations

import argparse
from pathlib import Path

import matplotlib

matplotlib.use("Agg")

import matplotlib.pyplot as plt
import pandas as pd


def load_results(path: Path) -> pd.DataFrame:
    df = pd.read_csv(path, sep="\t")
    df["status"] = df["status"].astype(str).str.lower()
    df["description"] = df["description"].fillna("").astype(str)
    df["commit"] = df["commit"].fillna("").astype(str)
    df["runtime_ms"] = pd.to_numeric(df["runtime_ms"], errors="coerce")
    df = df.dropna(subset=["runtime_ms"]).reset_index(drop=True)
    df["experiment"] = df.index
    return df


def truncate(text: str, limit: int = 46) -> str:
    return text if len(text) <= limit else text[: limit - 3] + "..."


def render_plot(df: pd.DataFrame, output_path: Path) -> None:
    valid = df[df["status"] != "crash"].copy()
    if valid.empty:
        raise SystemExit("No non-crash experiments found in results file.")

    kept = valid[valid["status"] == "keep"].copy()
    discards = valid[valid["status"] == "discard"].copy()

    baseline = float(valid.iloc[0]["runtime_ms"])
    best = float(kept["runtime_ms"].min() if not kept.empty else valid["runtime_ms"].min())
    worst = float(valid["runtime_ms"].max())
    y_pad = max((worst - best) * 0.15, 0.05)
    y_min = best - y_pad
    y_max = worst + y_pad
    if y_max <= y_min:
        y_max = y_min + 1.0

    plt.style.use("default")
    fig, ax = plt.subplots(figsize=(16, 8), dpi=150)

    ax.set_facecolor("white")
    fig.patch.set_facecolor("white")
    ax.grid(axis="y", color="black", alpha=0.12, linewidth=1)
    ax.grid(axis="x", visible=False)

    if not discards.empty:
        ax.scatter(
            discards["experiment"],
            discards["runtime_ms"],
            s=28,
            color="#c8c8c8",
            alpha=0.55,
            linewidths=0,
            zorder=2,
            label="Discarded",
        )

    if not kept.empty:
        frontier = kept[["experiment", "runtime_ms"]].copy()
        frontier["running_best"] = frontier["runtime_ms"].cummin()
        ax.step(
            frontier["experiment"],
            frontier["running_best"],
            where="post",
            color="#27ae60",
            linewidth=3.0,
            alpha=0.72,
            zorder=3,
            label="Running best",
        )

        ax.scatter(
            kept["experiment"],
            kept["runtime_ms"],
            s=62,
            color="#2ecc71",
            edgecolors="black",
            linewidths=1.0,
            zorder=4,
            label="Kept",
        )

        for _, row in kept.iterrows():
            ax.annotate(
                truncate(row["description"]),
                (row["experiment"], row["runtime_ms"]),
                xytext=(8, -6),
                textcoords="offset points",
                rotation=30,
                rotation_mode="anchor",
                fontsize=9,
                color="#1a7a3a",
                alpha=0.92,
                zorder=5,
            )

    ax.set_ylim(y_min, y_max)
    ax.set_xlim(-0.5, max(1, int(valid["experiment"].max())) + 0.5)
    ax.set_xlabel("Experiment #", fontsize=14)
    ax.set_ylabel("Runtime (ms, lower is better)", fontsize=14)
    ax.set_title(
        f"Autoresearch Progress: {len(df)} Experiments, {len(kept)} Kept Improvements",
        fontsize=20,
        pad=18,
    )

    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.spines["left"].set_color("#222")
    ax.spines["bottom"].set_color("#222")
    ax.tick_params(axis="both", labelsize=11, colors="#444")

    xtick_count = min(10, max(2, len(valid)))
    ax.set_xticks(
        sorted(
            {
                int(round(value))
                for value in pd.Series(range(xtick_count + 1))
                .mul(valid["experiment"].max() / xtick_count if xtick_count else 0)
            }
        )
    )

    handles, labels = ax.get_legend_handles_labels()
    if handles:
        legend = ax.legend(
            handles,
            labels,
            loc="upper right",
            frameon=True,
            framealpha=0.9,
            facecolor="white",
            edgecolor="#dddddd",
        )
        for text in legend.get_texts():
            text.set_color("#333")

    fig.tight_layout()
    fig.savefig(output_path, bbox_inches="tight")
    plt.close(fig)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Render benches/results.tsv to a Karpathy-style progress chart."
    )
    parser.add_argument(
        "--input",
        default="results.tsv",
        help="Input TSV file relative to the script directory. Default: results.tsv",
    )
    parser.add_argument(
        "--output",
        default="progress.svg",
        help="Output chart path relative to the script directory. Default: progress.svg",
    )
    args = parser.parse_args()

    base_dir = Path(__file__).resolve().parent
    input_path = (base_dir / args.input).resolve()
    output_path = (base_dir / args.output).resolve()
    df = load_results(input_path)
    render_plot(df, output_path)
    print(f"Saved {output_path}")


if __name__ == "__main__":
    main()
