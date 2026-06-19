# Simple single-panel positioning bar chart: reflect vs the LOCOMO field.
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import Patch

IVORY="#FAF9F5"; SLATE="#141413"; CLAY="#D97757"; OLIVE="#788C5D"
OAT="#E3DACC"; GRAY="#B9B7AD"; GRAY7="#3D3D3A"

data = [
    ("ByteRover 2.0", 96.1, "other"),
    ("Backboard", 90.0, "other"),
    ("Honcho", 89.9, "other"),
    ("Hindsight (Gemini-3)", 89.6, "hind"),
    ("Hindsight (OSS-120B)", 85.7, "hind"),
    ("Hindsight (OSS-20B)", 83.2, "hind"),
    ("reflect 4.1.0 + fixes", 77.5, "reflect"),
    ("Memobase", 75.8, "other"),
    ("Zep", 75.1, "other"),
    ("Mem0-Graph", 68.4, "other"),
    ("Mem0", 66.9, "other"),
    ("LangMem", 58.1, "other"),
    ("OpenAI", 52.9, "other"),
]
data.sort(key=lambda x: x[1])
names=[d[0] for d in data]; vals=[d[1] for d in data]
col={"hind":OLIVE,"reflect":CLAY,"other":GRAY}
cols=[col[d[2]] for d in data]

fig, ax = plt.subplots(figsize=(9.5, 6))
fig.patch.set_facecolor(IVORY); ax.set_facecolor(IVORY)
bars=ax.barh(names, vals, color=cols, edgecolor=SLATE, linewidth=0.6, zorder=3)
for b,d in zip(bars,data):
    ax.text(d[1]+0.8, b.get_y()+b.get_height()/2, f"{d[1]:.1f}", va="center",
            fontsize=10.5, color=SLATE, fontweight="bold" if d[2]=="reflect" else "normal")
# emphasise the reflect label
for lbl,d in zip(ax.get_yticklabels(), data):
    if d[2]=="reflect": lbl.set_fontweight("bold"); lbl.set_color(CLAY)

ax.set_xlim(0,100)
ax.set_xlabel("LOCOMO overall LLM-judge J  (%, 4 categories)", fontsize=11.5, color=GRAY7)
ax.set_title("Where reflect 4.1.0 sits on LOCOMO", fontsize=17, color=SLATE,
             fontweight="bold", loc="left", pad=12)
ax.axvline(72.9, color=SLATE, ls="--", lw=1, alpha=0.45, zorder=2)
ax.text(72.9, -0.55, "full-context 72.9", fontsize=8.5, color=GRAY7, alpha=0.8, ha="center")
for s in ["top","right"]: ax.spines[s].set_visible(False)
ax.tick_params(colors=GRAY7, labelsize=11)
ax.grid(axis="x", color=OAT, lw=0.8, zorder=0)
ax.legend(handles=[Patch(facecolor=OLIVE,label="Hindsight family"),
                   Patch(facecolor=CLAY,label="reflect 4.1.0 + fixes (our pilot)"),
                   Patch(facecolor=GRAY,label="other published")],
          loc="lower right", fontsize=9.5, frameon=False)
fig.text(0.5, -0.02,
    "⚠ Mixed harnesses/judges — NOT one ranking. Top systems (ByteRover/Honcho/Backboard) are "
    "self-reported on their own harness; field = Hindsight repo's judge; reflect = Opus judge, "
    "preliminary pilot. Same Zep reads 75 (Hindsight) vs 66 (Mem0 paper) — that gap is the noise.",
    ha="center", fontsize=8.0, color=GRAY7)
plt.tight_layout()
fig.savefig("results/locomo_positioning.png", dpi=150, facecolor=IVORY, bbox_inches="tight")
print("wrote results/locomo_positioning.png")
