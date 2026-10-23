#!/usr/bin/env python3
"""Plot recorded soak RSS and cumulative CPU with Matplotlib; never alters a test."""
import argparse
import json
import os
from pathlib import Path
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--desktop", type=Path, required=True, help="Soak evidence directory")
    parser.add_argument("--minikube", type=Path, required=True, help="Soak evidence directory")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    records = []
    for label, directory in [("Docker Desktop", args.desktop), ("minikube", args.minikube)]:
        report = json.loads((directory / "report.json").read_text())
        if not report.get("passed"):
            raise ValueError("Only completed passing soak reports may be plotted")
        source = directory / "samples.jsonl"
        if source.stat().st_size > 10 * 1024 * 1024:
            raise ValueError("Sample file exceeds plot input budget")
        samples = [json.loads(line) for line in source.read_text().splitlines() if line]
        if len(samples) < 2:
            raise ValueError("At least two samples are required")
        records.append((label, report, samples))
    if len({report["binary_sha256"] for _, report, _ in records}) != 1:
        raise ValueError("The two reports must identify the same binary")
    # Matplotlib's temporary configuration/cache never touches a user's settings.
    with tempfile.TemporaryDirectory(prefix="kborg-soak-plot-") as temporary:
        os.environ["MPLCONFIGDIR"] = temporary
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.pyplot as plt
        plt.rcParams.update({"font.family": "DejaVu Sans", "font.size": 10,
            "figure.facecolor": "#191724", "axes.facecolor": "#1f1d2e",
            "text.color": "#e0def4", "axes.labelcolor": "#e0def4",
            "xtick.color": "#908caa", "ytick.color": "#908caa",
            "axes.edgecolor": "#524f67", "grid.color": "#403d52",
            "savefig.facecolor": "#191724"})
        figure, axes = plt.subplots(2, 2, figsize=(13, 7.5), sharex="col")
        colors = {"resource": "#9ccfd8", "dashboard": "#c4a7e7", "tui": "#f6c177"}
        labels = {"resource": "Resource watch", "dashboard": "Dashboard stream", "tui": "Terminal UI"}
        for column, (label, report, samples) in enumerate(records):
            minutes = [row["elapsed_seconds"] / 60 for row in samples]
            for name, color in colors.items():
                axes[0, column].plot(minutes, [row["processes"][name]["rss_kib"] / 1024 for row in samples],
                                     color=color, linewidth=1.6, label=labels[name])
                first_cpu = samples[0]["processes"][name]["cpu_seconds"]
                axes[1, column].plot(minutes, [row["processes"][name]["cpu_seconds"] - first_cpu for row in samples],
                                     color=color, linewidth=1.6)
            axes[0, column].set_title(label, color="#e0def4", loc="left", fontsize=13, pad=12)
            axes[0, column].set_ylim(bottom=0)
            axes[1, column].set_ylim(bottom=0)
            axes[1, column].set_xlabel("Elapsed minutes")
            for row in range(2):
                axis = axes[row, column]
                axis.set_xlim(0, report["actual_continuous_seconds"] / 60)
                axis.grid(alpha=.5, linewidth=.6)
                axis.spines[["top", "right"]].set_visible(False)
                fault = report.get("fault")
                if fault and "resumed_seconds" in fault:
                    axis.axvspan(fault["start_seconds"] / 60, fault["resumed_seconds"] / 60,
                                 color="#eb6f92", alpha=.22)
            if report.get("fault"):
                axes[0, column].text(.98, .97, "Shaded: owned-cluster pause", transform=axes[0, column].transAxes,
                                     va="top", ha="right", fontsize=8, color="#ebbcba")
        axes[0, 0].set_ylabel("Resident memory · MiB")
        axes[1, 0].set_ylabel("CPU time since first sample · seconds")
        handles, names = axes[0, 0].get_legend_handles_labels()
        legend = figure.legend(handles, names, loc="upper left", bbox_to_anchor=(.06, .935),
                               ncol=3, frameon=False, fontsize=10)
        for text in legend.get_texts():
            text.set_color("#e0def4")
        figure.suptitle("KBorg · continuous local-cluster soak", x=.065, ha="left", fontsize=18, y=.97)
        figure.text(.065, .025, "Three persistent processes per cluster · private read-only identities · owned fixture changes only\n"
                    "RSS samples include allocator/platform variation. These measurements describe this bounded workload, not production-scale guarantees.",
                    fontsize=9, color="#908caa", va="bottom")
        figure.subplots_adjust(left=.065, right=.97, bottom=.15, top=.83, hspace=.26, wspace=.2)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        figure.savefig(args.output, dpi=150)
        plt.close(figure)


if __name__ == "__main__":
    main()
