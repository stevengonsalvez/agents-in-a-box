# Plot LOCOMO leaderboard (Hindsight harness) + reflect (tuned, our pilot) alongside.
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import Patch
import numpy as np

IVORY="#FAF9F5"; SLATE="#141413"; CLAY="#D97757"; OLIVE="#788C5D"
OAT="#E3DACC"; GRAY="#B9B7AD"; GRAY7="#3D3D3A"

# Overall LOCOMO J% — Hindsight repo leaderboard (its judge, full locomo10,
# adversarial excluded). reflect = our tuned config, Opus judge, 1 convo / 50 QA,
# 4-cat mean (single 0.80 / multi 0.80 / temporal 0.80 / open 0.70) = 76.2.
overall = [
    ("Backboard", 90.00, "other"),
    ("Hindsight (Gemini-3)", 89.61, "hind"),
    ("Hindsight (OSS-120B)", 85.67, "hind"),
    ("Hindsight (OSS-20B)", 83.18, "hind"),
    ("Memobase", 75.78, "other"),
    ("reflect 4.1.0+fixes (pilot)", 76.25, "reflect"),
    ("Zep", 75.14, "other"),
    ("Mem0-Graph", 68.44, "other"),
    ("Mem0", 66.88, "other"),
    ("LangMem", 58.10, "other"),
    ("OpenAI", 52.90, "other"),
]
overall.sort(key=lambda x: x[1])
names=[x[0] for x in overall]; vals=[x[1] for x in overall]
col={"hind":OLIVE,"reflect":CLAY,"other":GRAY}
cols=[col[x[2]] for x in overall]

fig, (ax1, ax2) = plt.subplots(
    2, 1, figsize=(11, 11), gridspec_kw={"height_ratios":[1.25,1]})
fig.patch.set_facecolor(IVORY)

ax1.set_facecolor(IVORY)
bars=ax1.barh(names, vals, color=cols, edgecolor=SLATE, linewidth=0.6, zorder=3)
for b,v,t in zip(bars,vals,[x[2] for x in overall]):
    ax1.text(v+0.8, b.get_y()+b.get_height()/2, f"{v:.1f}", va="center",
             fontsize=10, color=SLATE, fontweight="bold" if t=="reflect" else "normal")
ax1.set_xlim(0,100); ax1.set_xlabel("Overall LLM-judge J  (%, 4 categories)", fontsize=11, color=GRAY7)
ax1.set_title("LOCOMO — overall memory-system leaderboard",
              fontsize=16, color=SLATE, fontweight="bold", loc="left", pad=10)
ax1.axvline(72.9, color=SLATE, ls="--", lw=1, alpha=0.5, zorder=2)
ax1.text(72.9, 0.2, " full-context 72.9", fontsize=8.5, color=GRAY7, alpha=0.8)
for s in ["top","right"]: ax1.spines[s].set_visible(False)
ax1.tick_params(colors=GRAY7, labelsize=10)
ax1.grid(axis="x", color=OAT, lw=0.8, zorder=0)
ax1.legend(handles=[Patch(facecolor=OLIVE,label="Hindsight family"),
                    Patch(facecolor=CLAY,label="reflect 4.1.0 + fixes (our pilot)"),
                    Patch(facecolor=GRAY,label="other published")],
           loc="lower right", fontsize=9, frameon=False)

cats=["single-hop","multi-hop","temporal","open-domain"]
percat={
    "Hindsight (Gemini-3)":[86.17,70.83,83.80,95.12],
    "Mem0":[67.13,51.15,55.51,72.93],
    "reflect 4.1.0 + fixes":[80,80,80,70],
}
pcol={"Hindsight (Gemini-3)":OLIVE,"Mem0":GRAY,"reflect 4.1.0 + fixes":CLAY}
x=np.arange(len(cats)); w=0.26
ax2.set_facecolor(IVORY)
for i,(name,v) in enumerate(percat.items()):
    ax2.bar(x+(i-1)*w, v, w, label=name, color=pcol[name], edgecolor=SLATE, linewidth=0.6, zorder=3)
ax2.set_xticks(x); ax2.set_xticklabels(cats, fontsize=10, color=GRAY7)
ax2.set_ylim(0,100); ax2.set_ylabel("J  (%)", fontsize=11, color=GRAY7)
ax2.set_title("Per-category — reflect (tuned) vs Mem0 vs Hindsight (best)",
              fontsize=13, color=SLATE, fontweight="bold", loc="left", pad=8)
for s in ["top","right"]: ax2.spines[s].set_visible(False)
ax2.tick_params(colors=GRAY7, labelsize=10)
ax2.grid(axis="y", color=OAT, lw=0.8, zorder=0)
ax2.legend(fontsize=9, frameon=False, loc="upper right")

fig.text(0.5, 0.012,
    "⚠ Not one harness: leaderboard = Hindsight repo's judge on full locomo10 (~1986 QA). "
    "reflect = OUR pilot, Opus judge, 1 conversation / 50 QA, tuned (bge embedder + HyDE). "
    "Different judge (Opus vs GPT-4o-mini) shifts scores ±15-20 — directional placement, not a ranking.",
    ha="center", fontsize=8.2, color=GRAY7, wrap=True)

plt.tight_layout(rect=[0,0.03,1,1])
out="results/locomo_comparison.png"
fig.savefig(out, dpi=150, facecolor=IVORY, bbox_inches="tight")
print("wrote", out)
