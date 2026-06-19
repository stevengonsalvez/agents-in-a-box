# Simple single-panel positioning bar chart: reflect vs the LOCOMO field.
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import Patch

IVORY="#FAF9F5"; SLATE="#141413"; CLAY="#D97757"; CLAY2="#E8A98F"; OLIVE="#788C5D"
OAT="#E3DACC"; GRAY="#B9B7AD"; GRAY7="#3D3D3A"

# reflect bars carry a ±6 band = 1 standard error at n=50 (the honest "firms up
# when the test is expanded" amount); competitors are point values from their
# own reports, so no band. Opus = reference judge; Sonnet judge grades harsher.
ERR = 6.0
data = [
    ("ByteRover 2.0", 96.1, "other", 0),
    ("Backboard", 90.0, "other", 0),
    ("Honcho", 89.9, "other", 0),
    ("Hindsight (Gemini-3)", 89.6, "hind", 0),
    ("Hindsight (OSS-120B)", 85.7, "hind", 0),
    ("Hindsight (OSS-20B)", 83.2, "hind", 0),
    ("reflect — Opus judge (prelim)", 77.5, "reflect", ERR),
    ("Memobase", 75.8, "other", 0),
    ("Zep", 75.1, "other", 0),
    ("reflect — Sonnet judge (prelim)", 70.0, "reflect2", ERR),
    ("Mem0-Graph", 68.4, "other", 0),
    ("Mem0", 66.9, "other", 0),
    ("LangMem", 58.1, "other", 0),
    ("OpenAI", 52.9, "other", 0),
]
data.sort(key=lambda x: x[1])
names=[d[0] for d in data]; vals=[d[1] for d in data]; errs=[d[3] for d in data]
col={"hind":OLIVE,"reflect":CLAY,"reflect2":CLAY2,"other":GRAY}
cols=[col[d[2]] for d in data]

fig, ax = plt.subplots(figsize=(9.5, 6.8))
fig.patch.set_facecolor(IVORY); ax.set_facecolor(IVORY)
bars=ax.barh(names, vals, color=cols, edgecolor=SLATE, linewidth=0.6, zorder=3,
             xerr=errs, error_kw=dict(ecolor=SLATE, elinewidth=1.2, capsize=4, alpha=0.7))
for b,d in zip(bars,data):
    is_ref = d[2] in ("reflect","reflect2")
    off = d[3] + 0.8  # clear the error bar
    txt = f"{d[1]:.1f}±{int(d[3])}" if d[3] else f"{d[1]:.1f}"
    ax.text(d[1]+off, b.get_y()+b.get_height()/2, txt, va="center",
            fontsize=10.5, color=SLATE, fontweight="bold" if is_ref else "normal")
# emphasise the reflect labels
for lbl,d in zip(ax.get_yticklabels(), data):
    if d[2] in ("reflect","reflect2"): lbl.set_fontweight("bold"); lbl.set_color(CLAY)

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
                   Patch(facecolor=CLAY,label="reflect — Opus judge (our pilot)"),
                   Patch(facecolor=CLAY2,label="reflect — Sonnet judge (harsher)"),
                   Patch(facecolor=GRAY,label="other published")],
          loc="lower right", fontsize=9, frameon=False)
fig.text(0.5, -0.03,
    "⚠ Mixed harnesses/judges — NOT one ranking. Top systems (ByteRover/Honcho/Backboard) self-reported "
    "on their own harness; field = Hindsight repo's judge. reflect = preliminary 50-Q pilot; ±6 = 1 SE at "
    "n=50 (firms up at full scale). Same Zep reads 75 (Hindsight) vs 66 (Mem0 paper) — that gap is the noise.",
    ha="center", fontsize=7.8, color=GRAY7)
plt.tight_layout()
fig.savefig("results/locomo_positioning.png", dpi=150, facecolor=IVORY, bbox_inches="tight")
print("wrote results/locomo_positioning.png")
