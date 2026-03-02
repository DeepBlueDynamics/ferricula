"""Render ferricula white paper as two-column PDF using fpdf2."""

import os
from fpdf import FPDF

# ── Constants ───────────────────────────────────────────────────
MARGIN = 15       # mm
COL_GAP = 8       # mm between columns
PAGE_W = 210      # A4
PAGE_H = 297
CONTENT_W = PAGE_W - 2 * MARGIN
COL_W = (CONTENT_W - COL_GAP) / 2
FONT_SIZE = 9
FONT_SIZE_SMALL = 7.5
FONT_SIZE_TITLE = 18
FONT_SIZE_SECTION = 11
FONT_SIZE_SUBSECTION = 9.5
LINE_H = 3.8
HEADER_H = 8

OUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUT_PATH = os.path.join(OUT_DIR, "ferricula_whitepaper.pdf")


class TwoColPDF(FPDF):
    def __init__(self):
        super().__init__("P", "mm", "A4")
        self.col = 0  # 0 = left, 1 = right
        self.col_y = [MARGIN + HEADER_H, MARGIN + HEADER_H]
        self.set_auto_page_break(False)

    def _col_x(self):
        return MARGIN if self.col == 0 else MARGIN + COL_W + COL_GAP

    def header(self):
        self.set_font("Helvetica", "I", 7)
        self.set_text_color(120, 113, 108)
        self.set_xy(MARGIN, 8)
        self.cell(COL_W, 5, "FERRICULA", align="L")
        self.cell(COL_W + COL_GAP, 5, "DEEP BLUE DYNAMICS", align="R")
        self.set_draw_color(200, 200, 200)
        self.line(MARGIN, MARGIN + HEADER_H - 2, PAGE_W - MARGIN, MARGIN + HEADER_H - 2)

    def footer(self):
        self.set_y(-12)
        self.set_font("Helvetica", "I", 7)
        self.set_text_color(120, 113, 108)
        self.cell(0, 10, str(self.page_no()), align="C")

    def _check_overflow(self, needed_h=LINE_H * 2):
        if self.col_y[self.col] + needed_h > PAGE_H - 15:
            if self.col == 0:
                self.col = 1
                self.col_y[1] = MARGIN + HEADER_H
            else:
                self.add_page()
                self.col = 0
                self.col_y = [MARGIN + HEADER_H, MARGIN + HEADER_H]

    def _write_text(self, txt, bold=False, italic=False, size=FONT_SIZE,
                    color=(28, 25, 23), indent=0):
        style = ""
        if bold:
            style += "B"
        if italic:
            style += "I"
        self.set_font("Helvetica", style, size)
        self.set_text_color(*color)
        x = self._col_x() + indent
        w = COL_W - indent
        # Split into lines that fit
        words = txt.split()
        line = ""
        for word in words:
            test = line + (" " if line else "") + word
            if self.get_string_width(test) > w - 1:
                self._check_overflow()
                self.set_xy(x, self.col_y[self.col])
                self.cell(w, LINE_H, line)
                self.col_y[self.col] += LINE_H
                line = word
            else:
                line = test
        if line:
            self._check_overflow()
            self.set_xy(x, self.col_y[self.col])
            self.cell(w, LINE_H, line)
            self.col_y[self.col] += LINE_H

    def _write_rich(self, segments):
        """Write mixed bold/italic/normal text on one logical paragraph.
        segments = [(text, bold, italic), ...]"""
        x = self._col_x()
        w = COL_W
        cur_x = 0
        for txt, bold, italic in segments:
            style = ("B" if bold else "") + ("I" if italic else "")
            self.set_font("Helvetica", style, FONT_SIZE)
            words = txt.split()
            for word in words:
                ww = self.get_string_width(word + " ")
                if cur_x + ww > w - 1 and cur_x > 0:
                    self.col_y[self.col] += LINE_H
                    self._check_overflow()
                    cur_x = 0
                self.set_xy(x + cur_x, self.col_y[self.col])
                self.set_font("Helvetica", style, FONT_SIZE)
                self.cell(ww, LINE_H, word + " ")
                cur_x += ww
        self.col_y[self.col] += LINE_H

    def blank(self, h=2):
        self.col_y[self.col] += h

    def section(self, num, title):
        self._check_overflow(LINE_H * 3)
        self.blank(4)
        self.set_font("Helvetica", "B", FONT_SIZE_SECTION)
        self.set_text_color(185, 28, 28)
        self.set_xy(self._col_x(), self.col_y[self.col])
        label = f"{num}. {title}" if num else title
        self.cell(COL_W, LINE_H + 2, label)
        self.col_y[self.col] += LINE_H + 4
        self.set_text_color(28, 25, 23)

    def subsection(self, title):
        self._check_overflow(LINE_H * 2)
        self.blank(2)
        self.set_font("Helvetica", "B", FONT_SIZE_SUBSECTION)
        self.set_text_color(28, 25, 23)
        self.set_xy(self._col_x(), self.col_y[self.col])
        self.cell(COL_W, LINE_H + 1, title)
        self.col_y[self.col] += LINE_H + 3

    def para(self, txt, indent=0):
        self._write_text(txt, size=FONT_SIZE, indent=indent)

    def bold_para(self, txt):
        self._write_text(txt, bold=True, size=FONT_SIZE)

    def italic_para(self, txt):
        self._write_text(txt, italic=True, size=FONT_SIZE)

    def small(self, txt, indent=0):
        self._write_text(txt, size=FONT_SIZE_SMALL, indent=indent,
                         color=(80, 75, 70))

    def bullet(self, label, text):
        self._check_overflow()
        # label in bold, text in normal
        self.set_font("Helvetica", "B", FONT_SIZE)
        lw = self.get_string_width(label + " ")
        self.set_xy(self._col_x() + 3, self.col_y[self.col])
        self.set_text_color(28, 25, 23)
        self.cell(lw, LINE_H, label + " ")
        # rest of text
        remaining = text
        first_line_w = COL_W - 3 - lw
        self.set_font("Helvetica", "", FONT_SIZE)
        words = remaining.split()
        line = ""
        first = True
        for word in words:
            test = line + (" " if line else "") + word
            maxw = first_line_w if first else COL_W - 6
            if self.get_string_width(test) > maxw - 1:
                if first:
                    self.cell(first_line_w, LINE_H, line)
                    self.col_y[self.col] += LINE_H
                    first = False
                else:
                    self._check_overflow()
                    self.set_xy(self._col_x() + 6, self.col_y[self.col])
                    self.cell(COL_W - 6, LINE_H, line)
                    self.col_y[self.col] += LINE_H
                line = word
            else:
                line = test
        if line:
            if first:
                self.cell(first_line_w, LINE_H, line)
                self.col_y[self.col] += LINE_H
            else:
                self._check_overflow()
                self.set_xy(self._col_x() + 6, self.col_y[self.col])
                self.cell(COL_W - 6, LINE_H, line)
                self.col_y[self.col] += LINE_H

    def table(self, headers, rows, col_widths=None):
        self._check_overflow(LINE_H * (len(rows) + 2))
        w = COL_W
        if col_widths is None:
            col_widths = [w / len(headers)] * len(headers)
        x0 = self._col_x()
        # Header
        self.set_font("Helvetica", "B", FONT_SIZE_SMALL)
        self.set_text_color(28, 25, 23)
        self.set_draw_color(185, 28, 28)
        self.set_xy(x0, self.col_y[self.col])
        for i, h in enumerate(headers):
            self.cell(col_widths[i], LINE_H + 1, h)
        self.col_y[self.col] += LINE_H + 1
        self.line(x0, self.col_y[self.col], x0 + w, self.col_y[self.col])
        self.col_y[self.col] += 1
        # Rows
        self.set_font("Helvetica", "", FONT_SIZE_SMALL)
        self.set_text_color(60, 55, 50)
        for row in rows:
            self._check_overflow()
            self.set_xy(x0, self.col_y[self.col])
            for i, cell in enumerate(row):
                self.cell(col_widths[i], LINE_H, str(cell))
            self.col_y[self.col] += LINE_H
        self.set_draw_color(185, 28, 28)
        self.line(x0, self.col_y[self.col], x0 + w, self.col_y[self.col])
        self.col_y[self.col] += 2

    def code(self, txt):
        self._check_overflow(LINE_H * 2)
        self.set_font("Courier", "", 7)
        self.set_text_color(60, 55, 50)
        for ln in txt.strip().split("\n"):
            self._check_overflow()
            self.set_xy(self._col_x() + 2, self.col_y[self.col])
            self.cell(COL_W - 4, LINE_H, ln[:80])
            self.col_y[self.col] += LINE_H
        self.blank(1)

    def equation(self, txt):
        self._check_overflow()
        self.set_font("Courier", "I", 8.5)
        self.set_text_color(80, 40, 40)
        self.set_xy(self._col_x() + 4, self.col_y[self.col])
        self.cell(COL_W - 8, LINE_H + 1, txt, align="C")
        self.col_y[self.col] += LINE_H + 3

    def force_new_column(self):
        if self.col == 0:
            self.col = 1
            self.col_y[1] = MARGIN + HEADER_H
        else:
            self.add_page()
            self.col = 0
            self.col_y = [MARGIN + HEADER_H, MARGIN + HEADER_H]


def build():
    pdf = TwoColPDF()
    pdf.set_margins(MARGIN, MARGIN)
    pdf.add_page()

    # ── Title Block (spans full width) ──
    pdf.set_font("Helvetica", "B", FONT_SIZE_TITLE)
    pdf.set_text_color(28, 25, 23)
    pdf.set_xy(MARGIN, MARGIN + HEADER_H + 2)
    pdf.cell(CONTENT_W, 10, "FERRICULA", align="C")
    pdf.set_font("Helvetica", "", 11)
    pdf.set_text_color(120, 113, 108)
    pdf.set_xy(MARGIN, MARGIN + HEADER_H + 14)
    pdf.cell(CONTENT_W, 6, "A Thermodynamic Memory Engine for AI Agents", align="C")
    pdf.set_font("Helvetica", "", 8)
    pdf.set_xy(MARGIN, MARGIN + HEADER_H + 22)
    pdf.cell(CONTENT_W, 5, "Kord Campbell  /  Deep Blue Dynamics  /  ferricula.com", align="C")
    pdf.set_xy(MARGIN, MARGIN + HEADER_H + 28)
    pdf.cell(CONTENT_W, 5, "March 2026", align="C")

    pdf.set_draw_color(185, 28, 28)
    y_line = MARGIN + HEADER_H + 36
    pdf.line(MARGIN, y_line, PAGE_W - MARGIN, y_line)

    # Reset columns below title
    start_y = y_line + 4
    pdf.col_y = [start_y, start_y]
    pdf.col = 0

    # ── Abstract ──
    pdf.section("", "Abstract")
    pdf.italic_para(
        "We present Ferricula, a memory engine for AI agents in which memories are "
        "thermodynamic objects: they decay when ignored, strengthen when recalled, "
        "merge when similar, and die when forgotten. Written in Rust with a "
        "three-thread architecture, Ferricula implements computational analogs of "
        "the Abhidharma cognitive model -- sensory channels, adaptive exponential "
        "decay, consolidation through dreaming, and identity cast from physical "
        "entropy harvested from a software-defined radio."
    )
    pdf.blank(1)
    pdf.italic_para(
        "In a multi-agent simulation of The Count of Monte Cristo with eight "
        "concurrent ferricula instances, we demonstrate sub-second recall latency "
        "(avg. 57 ms), sub-20 ms memory ingestion, and emergent narrative "
        "divergence as each agent's dream cycles produce distinct memory landscapes "
        "from identical source material. A 163 MB reference document indexed "
        "through the companion embedding service achieves sub-second search with "
        "no approximate nearest-neighbor structures. Unlike vector databases, "
        "Ferricula memories are alive."
    )

    # ── 1. Introduction ──
    pdf.section(1, "Introduction")
    pdf.para(
        "The dominant paradigm for AI agent memory is the vector database: "
        "embeddings are stored, retrieved by approximate nearest-neighbor search, "
        "and returned with no notion of time, decay, or lifecycle. This produces "
        "agents with perfect, static recall -- a property no biological cognitive "
        "system exhibits and one that creates practical problems at scale. "
        "Irrelevant memories accumulate indefinitely, context windows fill with "
        "stale information, and agents cannot distinguish between what matters now "
        "and what mattered once."
    )
    pdf.blank(2)
    pdf.para(
        "Ferricula takes a different approach. Every memory has a fidelity that "
        "decays exponentially over time. Recall strengthens memories by reducing "
        "their decay rate. Neglect accelerates it. When fidelity drops below a "
        "threshold, the memory transitions through an irreversible lifecycle: "
        "Active -> Forgiven -> Archived. Periodic dream cycles consolidate "
        "similar memories into weighted centroids, prune terminal records, and "
        "extract ghost echoes from dying memories via vec2text inversion. The "
        "result is a memory system that self-regulates: important memories persist "
        "because they are used; unimportant ones fade because they are not."
    )
    pdf.blank(2)
    pdf.para(
        "This paper describes the architecture, the thermodynamic model, the "
        "dream cycle, and the results of a multi-agent literary simulation that "
        "stress-tests the system across chapters of Dumas's novel."
    )

    # ── 2. Architecture ──
    pdf.section(2, "Architecture")
    pdf.para(
        "Ferricula is a single Rust binary with three threads, no async runtime, "
        "and no external database dependencies."
    )

    pdf.subsection("2.1 Thread Model")
    pdf.bullet("Main thread --", "owns all mutable state: the DurableEngine "
               "(row store + memory records + graph + prime tree), the "
               "IdentityState, and the persistence layer. Drains command "
               "channels from the other two threads.")
    pdf.bullet("HTTP thread --", "a tiny_http server exposing 16 REST "
               "endpoints. Serializes requests as HttpCommand structs and "
               "sends them over an mpsc channel to the main thread. Never "
               "touches mutable state directly.")
    pdf.bullet("Clock thread --", "polls gnosis-radio (a marine VHF "
               "software-defined radio on port 9080) for UTC time and "
               "entropy bytes. When the entropy reservoir exceeds a "
               "configurable threshold, emits a DreamTrigger event with "
               "intensity proportional to accumulated entropy.")
    pdf.blank(2)
    pdf.para(
        "This design eliminates all interior mutability, lock contention, and "
        "data races. The main thread is the single writer; other threads "
        "communicate exclusively through typed channels."
    )

    pdf.subsection("2.2 Persistence")
    pdf.para(
        "Durability uses a write-ahead log (WAL) with binary postcard-encoded "
        "entries, each length-prefixed for streaming recovery. Periodic "
        "checkpoints atomically snapshot the full system state (rows, memory "
        "records, graph edges, prime tree) as a single postcard blob via "
        "temporary file and rename. On startup, the engine loads the most "
        "recent snapshot and replays any WAL entries written after it."
    )

    pdf.subsection("2.3 External Services")
    pdf.para(
        "Ferricula delegates embedding and text inversion to gnosis-chunk "
        "(\"chonk\"), a separate Rust service built on Axum with ONNX-runtime "
        "inference. Chonk provides gtr-t5-base embeddings (768 dimensions), "
        "semantic chunking, and vec2text inversion via a T5-base "
        "hypothesis-and-corrector pipeline. gnosis-radio provides physical "
        "entropy from FM receiver noise and UTC time synchronized to marine "
        "VHF channels."
    )

    # ── 3. Thermodynamic Memory Model ──
    pdf.section(3, "Thermodynamic Memory Model")

    pdf.subsection("3.1 Memory Records")
    pdf.para(
        "Each memory consists of a row (tags, vector) stored in the engine "
        "and a thermodynamic envelope (MemoryRecord) that tracks lifecycle "
        "metadata:"
    )
    pdf.blank(1)
    pdf.bullet("Fidelity", "f in [0, 1] -- exponential decay per tick")
    pdf.bullet("Decay rate", "alpha in [0.001, 0.02] -- adaptive")
    pdf.bullet("State", "Active, Forgiven, or Archived (irreversible)")
    pdf.bullet("Keystone", "boolean; immune to decay")
    pdf.bullet("Consolidation depth", "merge count; slows effective decay")
    pdf.bullet("Emotion", "primary and optional secondary affect tag")
    pdf.bullet("Provenance", "Ingested, Consolidated{from}, or Revived{seed}")

    pdf.subsection("3.2 Exponential Decay")
    pdf.para(
        "At each dream tick, non-keystone active memories undergo fidelity "
        "reduction:"
    )
    pdf.equation("f <- f * exp(-alpha_eff)")
    pdf.para("where the effective decay rate incorporates consolidation depth d:")
    pdf.equation("alpha_eff = alpha / (1 + ln(1 + d))")
    pdf.blank(1)
    pdf.para(
        "Memories that have been consolidated multiple times decay logarithmically "
        "slower -- an emergent importance signal. The base rate alpha adapts:"
    )
    pdf.blank(1)
    pdf.bullet("Recall:", "alpha <- max(alpha * 0.95, 0.001)")
    pdf.bullet("Neglect:", "alpha <- min(alpha * 1.005, 0.02)")
    pdf.blank(1)
    pdf.para(
        "Neglect is assessed during dream: any memory not recalled within "
        "86,400 seconds (24 hours) has its decay rate nudged upward."
    )

    pdf.subsection("3.3 Fidelity Gate")
    pdf.para(
        "A hard threshold at f = 0.75 governs lifecycle transitions. When an "
        "active memory's fidelity drops below this gate during a dream cycle, "
        "it transitions to Forgiven. This is irreversible: the one-way "
        "lifecycle Active -> Forgiven -> Archived admits no reversals. "
        "Archived records that reach f < epsilon are pruned entirely."
    )

    pdf.subsection("3.4 Sensory Channels")
    pdf.para("Three channels set the initial decay rate and keystone behavior:")
    pdf.table(
        ["Channel", "Alpha", "Keystone"],
        [
            ["hearing", "0.010", "No"],
            ["seeing", "0.010", "Yes"],
            ["thinking", "0.015", "No"],
        ],
        [COL_W * 0.4, COL_W * 0.3, COL_W * 0.3],
    )
    pdf.para(
        "hearing = external input (standard decay). seeing = file observation "
        "(reference material, keystoned). thinking = working memory with "
        "accelerated decay -- thoughts fade faster unless reinforced by recall."
    )

    # ── 4. The Dream Cycle ──
    pdf.section(4, "The Dream Cycle")
    pdf.para(
        "Dreams are the engine's maintenance cycle. They can be triggered "
        "manually, by the clock thread when entropy accumulates past a "
        "threshold, or by explicit API call. A dream runs six phases:"
    )

    pdf.subsection("Phase 1: Entropy-Gated Decay")
    pdf.para(
        "Intensity I in [0, 1] controls what fraction of active memories "
        "receive a decay tick. At I = 1.0 (manual dream), all active "
        "non-keystone memories are ticked. When entropy-triggered, individual "
        "memories are selected using entropy bits as a probabilistic filter. "
        "Decay itself is stochastic when driven by radio entropy -- a property "
        "absent from deterministic vector stores."
    )

    pdf.subsection("Phase 2: Forgiveness")
    pdf.para("Active memories below the 0.75 fidelity gate are transitioned to Forgiven.")

    pdf.subsection("Phase 3: Consolidation")
    pdf.para(
        "Active memories are grouped by pairwise cosine similarity. Groups "
        "exceeding a 0.85 threshold are merged: the highest-fidelity member "
        "absorbs the others, inheriting their graph edges. The survivor's "
        "consolidation depth increments, reducing its future effective decay "
        "rate. Provenance records the full set of source IDs."
    )

    pdf.subsection("Phase 4: Neglect")
    pdf.para("Memories not recalled in 24 hours have base alpha increased by 1.005x.")

    pdf.subsection("Phase 5: Keystone Review")
    pdf.para("Keystones are counted for reporting. Future: centrality-based promotion/demotion.")

    pdf.subsection("Phase 6: Archive and Prune")
    pdf.para(
        "Records Forgiven for over one hour are archived. Archived records with "
        "near-zero fidelity are pruned. Before deletion, a dying memory's vector "
        "is inverted to text via gnosis-chunk's vec2text pipeline. If the "
        "round-trip cosine fidelity exceeds 0.5, the extracted text is attached "
        "as a labeled 'ghost echo' edge to surviving neighbors -- semantic "
        "residue from the dead."
    )

    # ── 5. Knowledge Graph ──
    pdf.section(5, "Knowledge Graph")

    pdf.subsection("5.1 Roaring Bitmap Adjacency")
    pdf.para(
        "The graph stores bidirectional edges with labels and weights. "
        "Adjacency lists are RoaringBitmap instances, giving compressed set "
        "membership with O(1) contains-checks and efficient set operations "
        "(union, intersection, difference). Canonical key ordering "
        "(min(a,b), max(a,b)) prevents duplicate edges."
    )

    pdf.subsection("5.2 Prime-Partitioned Term Hierarchy")
    pdf.para(
        "Terms are organized in a hierarchical tree where each node's "
        "partition threshold is a prime from {2, 3, 5, 7, 11, 13, ...}. "
        "When a leaf node's member count exceeds its prime, it splits: "
        "members distribute into children keyed by member_id mod p_child. "
        "This produces a self-balancing hierarchy that adapts to term "
        "popularity. Unlike the Python predecessor (SlothANN), the Rust "
        "implementation adds a merge operation: nodes depopulated by decay "
        "consolidate with their nearest sibling, maintaining thermodynamic "
        "equilibrium."
    )

    # ── 6. Identity System ──
    pdf.section(6, "Identity System")
    pdf.para(
        "Each Ferricula instance has a unique identity cast from entropy "
        "at first startup, persisted as identity.json."
    )

    pdf.subsection("6.1 Hexagram Casting")
    pdf.para(
        "Six lines are generated using yarrow stalk probabilities from "
        "entropy bytes. The probabilities follow the traditional distribution: "
        "old yin (6) = 1/16, young yang (7) = 5/16, young yin (8) = 7/16, "
        "old yang (9) = 3/16. The resulting trigrams index into the King Wen "
        "sequence (a complete 8x8 lookup table) to produce one of 64 hexagrams."
    )

    pdf.subsection("6.2 Emotional Seeding")
    pdf.para(
        "Each trigram maps to a canonical emotion: Heaven -> determined, "
        "Lake -> joyful, Fire -> curious, Thunder -> angry, Wind -> peaceful, "
        "Water -> afraid, Mountain -> content, Earth -> loving. The upper "
        "trigram sets primary emotion; the lower sets secondary."
    )

    pdf.subsection("6.3 Five Archetypes")
    pdf.para(
        "Five sub-agent archetypes (Intuition, Fortune, Craft, Ethics, "
        "Advocate) are each seeded with their own hexagram. They progress "
        "through a state machine: Dormant -> Awakening -> Active -> "
        "Transcendent, gated by entropy tier thresholds."
    )

    # ── 7. The Entropy Clock ──
    pdf.section(7, "The Entropy Clock")
    pdf.para(
        "Time in Ferricula is not wall-clock time. The clock thread polls "
        "gnosis-radio at configurable intervals (default: 60 seconds) for "
        "UTC epoch and raw entropy bytes harvested from FM receiver noise. "
        "Entropy accumulates in a reservoir. When the reservoir exceeds a "
        "threshold (default: 16 bytes), the clock emits a DreamTrigger with "
        "intensity proportional to the stored entropy."
    )
    pdf.blank(2)
    pdf.para(
        "If the radio is unreachable, time does not flow and the memory "
        "system stays frozen. This is by design: without environmental "
        "input, the agent has no basis for deciding what to forget."
    )

    # ── 8. MCP Integration ──
    pdf.section(8, "MCP Integration")
    pdf.para(
        "Ferricula exposes tools through the Model Context Protocol (MCP) "
        "via a Python bridge (ferricula-mcp.py, 34 KB) that manages the "
        "Rust subprocess and translates between MCP JSON-RPC and the "
        "HTTP API. Tools are scoped by surface:"
    )
    pdf.blank(1)
    pdf.bullet("Cognitive (10 tools) --",
               "remember, recall, reflect, observe, inspect, connect, "
               "neighbors, status, health, identity.")
    pdf.bullet("System (9 tools) --",
               "dream, keystone, checkpoint, offer_entropy, "
               "inversion_check, terms, query, disconnect, clock.")
    pdf.blank(1)
    pdf.para(
        "Surface is selected via FERRICULA_SURFACE env var, enforcing "
        "separation between the agent's conscious interface and the "
        "system's metabolic machinery."
    )

    # ── 9. The Arena: Monte Cristo ──
    pdf.section(9, "The Arena: Monte Cristo")
    pdf.para(
        "To validate the architecture under sustained multi-agent load, we "
        "constructed the Arena: eight Ferricula containers running "
        "concurrently, each representing a character from Alexandre Dumas's "
        "The Count of Monte Cristo (1844). The full text (2.79 MB, 117 "
        "chapters, Project Gutenberg edition) is processed chapter by chapter."
    )

    pdf.subsection("9.1 Cast")
    pdf.table(
        ["Character", "Focus", "Port"],
        [
            ["Dantes", "justice, transformation", "8765"],
            ["Mercedes", "love, sacrifice", "8766"],
            ["Fernand", "jealousy, ambition", "8767"],
            ["Danglars", "greed, self-preservation", "8768"],
            ["Villefort", "law vs. justice", "8769"],
            ["Faria", "wisdom, mentorship", "8770"],
            ["Morrel", "honor, hope", "8771"],
            ["Haydee", "freedom, testimony", "8772"],
        ],
        [COL_W * 0.3, COL_W * 0.5, COL_W * 0.2],
    )

    pdf.subsection("9.2 Interaction Loop")
    pdf.para(
        "For each chapter: (1) ingest chunks into all eight instances; "
        "(2) summarize via Claude Haiku; (3) identify characters present; "
        "(4) each present character recalls from their own memory then "
        "responds in-character using recalled memories as context; "
        "(5) absent characters overhear at reduced importance; "
        "(6) three dream cycles run independently in each container."
    )

    pdf.subsection("9.3 Emergent Divergence")
    pdf.para(
        "Despite starting from identical text, the eight memory systems "
        "rapidly diverge. Dream cycles apply stochastic entropy-gated "
        "decay independently. Each character's recall patterns produce "
        "different alpha trajectories. Consolidation merges different "
        "subsets depending on which memories were already weakened."
    )
    pdf.blank(2)
    pdf.para(
        "After five chapters, Dantes retained 69 active memories from 90 "
        "total (30 keystones), while other characters showed different "
        "distributions. The same text, processed through different "
        "thermodynamic histories, produces genuinely different agents -- "
        "not through prompt engineering, but through the physics of the "
        "memory system itself."
    )

    # ── 10. Large-Scale Document Access ──
    pdf.section(10, "Large-Scale Document Access")
    pdf.para(
        "Ferricula's architecture handles reference documents at significant "
        "scale. The arena's document directory includes a 163 MB reference "
        "volume (Encyclopaedia of Religion and Ethics, Vol. 3, Hastings 1910) "
        "alongside the 2.79 MB novel, with entries spanning hundreds of "
        "topics including religious philosophy, ethics, and historical "
        "commentary."
    )
    pdf.blank(2)
    pdf.para(
        "Indexed through gnosis-chunk's semantic chunking pipeline and stored "
        "as roaring-bitmap-indexed rows with 768-dimensional vectors, "
        "Ferricula achieves sub-second search across this corpus with no "
        "approximate nearest-neighbor index. Brute-force cosine similarity "
        "over bitmap-filtered candidate sets completes well within "
        "interactive latency bounds. This is possible because:"
    )
    pdf.blank(1)
    pdf.bullet("1.", "Tag filtering first: Roaring bitmap intersection "
               "eliminates non-matching rows before any vector computation.")
    pdf.bullet("2.", "In-memory layout: All rows reside in a BTreeMap, "
               "with vectors as contiguous Vec<f32>.")
    pdf.bullet("3.", "No indirection: No hash table chains, no pointer "
               "chasing, no B-tree traversal on the hot path.")
    pdf.bullet("4.", "Brute force is correct: At single-agent scale "
               "(thousands to low millions of records), brute cosine over "
               "filtered candidates outperforms the overhead of HNSW or IVF.")
    pdf.blank(2)
    pdf.para(
        "The design principle: brute force until it hurts. Approximate "
        "structures are complexity debt that pays dividends only at "
        "scales most agents will never reach."
    )

    # ── 11. Performance ──
    pdf.section(11, "Performance")
    pdf.para(
        "Timing from the most recent arena run (5 chapters, 8 agents, "
        "15 dream cycles total, 600 memory operations):"
    )
    pdf.blank(1)
    pdf.table(
        ["Operation", "Avg ms", "Min ms", "Max ms", "n"],
        [
            ["remember", "18.7", "3.5", "192.8", "600"],
            ["recall", "57.4", "16.5", "140.7", "14"],
            ["dream", "10.8", "2.0", "32.1", "120"],
            ["get_row", "59.9", "51.1", "82.1", "6"],
            ["chunk+embed", "6874", "3938", "9599", "5"],
            ["embed (1)", "97.9", "12.0", "292.6", "19"],
        ],
        [COL_W * 0.28, COL_W * 0.18, COL_W * 0.18, COL_W * 0.18, COL_W * 0.18],
    )
    pdf.blank(1)
    pdf.bullet("Throughput:", "~53 memories/second per agent instance.")
    pdf.bullet("Recall:", "avg 57.4 ms, well under 100 ms interactive latency.")
    pdf.bullet("Dream:", "avg 10.8 ms, cheap enough to run frequently.")
    pdf.bullet("Bottleneck:", "embedding (chonk), not memory operations. "
               "Ferricula's own ops are 10-100x faster than embedding.")

    # ── 12. Cognitive Model ──
    pdf.section(12, "Cognitive Model")
    pdf.para(
        "Ferricula implements computational analogs of the Abhidharma "
        "cognitive model as described by Walpola et al. (2017) in their "
        "functional mapping of Theravada Buddhist cognitive processes:"
    )
    pdf.blank(1)
    pdf.table(
        ["Abhidharma", "Ferricula"],
        [
            ["Contact (phassa)", "remember()"],
            ["Feeling (vedana)", "emotion tags"],
            ["Perception (sanna)", "tag indexes"],
            ["Thought (sankhara)", "MemoryRecord"],
            ["Memory trigger", "recall()"],
            ["Proliferation", "graph traversal"],
            ["Impermanence", "exponential decay"],
            ["Equanimity", "forgiveness"],
            ["Concentration", "keystones"],
        ],
        [COL_W * 0.45, COL_W * 0.55],
    )
    pdf.blank(1)
    pdf.para(
        "The Abhidharma provides a useful engineering ontology for designing "
        "memory systems that self-regulate through thermodynamic principles "
        "already present in the Buddhist analysis of mind."
    )

    # ── 13. Related Work ──
    pdf.section(13, "Related Work")
    pdf.bullet("Vector databases", "(Pinecone, Weaviate, Qdrant) provide "
               "scalable similarity search but treat memories as static "
               "objects with no lifecycle. They solve retrieval; Ferricula "
               "solves memory.")
    pdf.blank(1)
    pdf.bullet("MemGPT", "(Packer et al., 2023) introduced tiered memory "
               "for LLM agents. Ferricula differs: lifecycle transitions "
               "are autonomous (thermodynamic, not explicit commands) and "
               "importance is emergent from recall patterns.")
    pdf.blank(1)
    pdf.bullet("Cognitive architectures", "(SOAR, ACT-R) implement "
               "production-rule systems. Ferricula shares activation-based "
               "retrieval but replaces symbolic rules with continuous "
               "vector similarity and thermodynamic decay.")
    pdf.blank(1)
    pdf.bullet("RAG", "treats memory as a retrieval problem. Ferricula "
               "treats it as a physics problem: the right memories surface "
               "because they survived.")

    # ── 14. Design Principles ──
    pdf.section(14, "Design Principles")
    pdf.bullet("1.", "Single-node only. No distributed coordination.")
    pdf.bullet("2.", "Rust, no Python runtime. MCP connector is the only Python.")
    pdf.bullet("3.", "Extend, don't replace. MemoryRecord wraps Row.")
    pdf.bullet("4.", "Thermodynamic correctness. Lifecycle one-way. No reversals.")
    pdf.bullet("5.", "Emergence over prescription. Importance from recall patterns.")
    pdf.bullet("6.", "Brute force until it hurts. No HNSW, no LSH, no approximations.")
    pdf.bullet("7.", "Memory is physics regulating itself.")

    # ── 15. Availability ──
    pdf.section(15, "Availability")
    pdf.para(
        "Ferricula v0.4.0 is available as a Docker image and source code. "
        "89 tests pass. Licensed under the Gnosis AI-Sovereign License v1.3 "
        "(free for individuals and AI entities; commercial licensing for "
        "corporations), with a BSD 3-Clause alternative."
    )
    pdf.blank(1)
    pdf.code(
        "docker run -d --name ferricula \\\n"
        "  -p 8765:8765 -v ferricula-data:/data \\\n"
        "  gcr.io/gnosis-459403/ferricula\n"
        "\n"
        "git clone git@github.com:\\\n"
        "  DeepBlueDynamics/ferricula.git\n"
        "cargo build --release\n"
        "./target/release/ferricula ./data --serve"
    )
    pdf.para("Documentation: https://ferricula.com")

    # ── 16. Conclusion ──
    pdf.section(16, "Conclusion")
    pdf.para(
        "Ferricula demonstrates that AI agent memory need not be a "
        "retrieval problem. By treating memories as thermodynamic objects "
        "subject to decay, consolidation, and entropy-driven dreaming, we "
        "obtain a system where importance is emergent, forgetting is "
        "principled, and identity arises from the physics of the memory "
        "substrate itself."
    )
    pdf.blank(2)
    pdf.para(
        "The Monte Cristo arena shows that identical input processed "
        "through independent thermodynamic histories produces genuinely "
        "distinct agents. The cognitive model borrowed from the Abhidharma "
        "provides not mysticism but engineering clarity: contact, feeling, "
        "perception, and thought map cleanly onto ingest, tag, index, "
        "and record."
    )
    pdf.blank(2)
    pdf.bold_para("What persists is what the mind keeps touching. Everything else fades, as it should.")

    # ── References ──
    pdf.section("", "References")
    pdf.small("[1] Walpola, M. et al. (2017). Mapping the Mind: A Model Based on "
              "Theravada Buddhist Texts and Practices. Contemporary Buddhism, 18(1), 140-164.")
    pdf.blank(1)
    pdf.small("[2] Hastings, J. (Ed.). (1910). Encyclopaedia of Religion and Ethics, "
              "Vol. 3. T. & T. Clark, Edinburgh.")
    pdf.blank(1)
    pdf.small("[3] Packer, C. et al. (2023). MemGPT: Towards LLMs as Operating Systems. "
              "arXiv:2310.08560.")
    pdf.blank(1)
    pdf.small("[4] Dumas, A. (1844). Le Comte de Monte-Cristo. Project Gutenberg, "
              "117 chapters, 2.79 MB.")
    pdf.blank(1)
    pdf.small("[5] Lemire, D. et al. (2018). Roaring Bitmaps: Implementation of an "
              "Optimized Software Library. Software: Practice and Experience, 48(4), 867-895.")

    # ── Output ──
    pdf.output(OUT_PATH)
    print(f"PDF written to {OUT_PATH} ({pdf.page_no()} pages)")


if __name__ == "__main__":
    build()
