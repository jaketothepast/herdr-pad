// Controller: what the Xbox controller (herdr-pad) is doing, on screen.
//
// herdr-pad owns all the logic and sends this mod its view as
// `controller.state <json>`; the mod only draws it. Shapes:
//   compact  a small pill: the last action (fades), and where X goes next
//   menu     the action menu: D-pad moves, A picks, B closes
//   legend   what every button does
//   layer    LT held: the spaces, LB / RB move between them
// Sized to read from the couch. Installed by `herdr-pad install`;
// edits here are overwritten on reinstall.

import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { agents, focused, nextCommand, spaces, theme } from "herdr";

const LABEL_MS = 3200;
const SIZE = { title: 20, body: 18, small: 14, key: 14 };
// Xbox face-button colors, so the badges read at a glance.
const FACE = { A: "#5dbb46", B: "#e2453c", X: "#3d8fe0", Y: "#f2b632" };

export default class Controller extends View {
  init(_props, cx) {
    this.view = null;
    this.labelSeq = -1;
    this.labelVisible = false;
    this.holdSeq = -1;
    this.holdFull = false;
    this.width = 0;
    cx.spawn(async (cx) => {
      for (;;) {
        const c = await nextCommand();
        if (!c.startsWith("state ")) continue;
        try {
          this.update(JSON.parse(c.slice(6)), cx);
        } catch (e) {
          console.log(`controller: bad state: ${e}`);
        }
        cx.notify();
      }
    });
  }

  update(v, cx) {
    this.view = v;
    if (v.label && v.seq !== this.labelSeq) {
      this.labelSeq = v.seq;
      this.labelVisible = true;
      const seq = v.seq;
      cx.timer.after(LABEL_MS, (cx) => {
        if (this.labelSeq === seq) {
          this.labelVisible = false;
          cx.notify();
        }
      });
    }
    // A hold bar starts empty, then fills natively over the hold time.
    if (v.hold && v.hold.seq !== this.holdSeq) {
      this.holdSeq = v.hold.seq;
      this.holdFull = false;
      cx.timer.after(20, (cx) => {
        this.holdFull = true;
        cx.notify();
      });
    }
    if (!v.hold) this.holdSeq = -1;
  }

  render(_cx) {
    const t = theme();
    const v = this.view;
    const panel = (width) =>
      v_flex()
        .id("controller")
        .when(width != null, (el) => el.w(width))
        .bg(t.raised)
        .border(1)
        .border_color(t.input)
        .rounded(12)
        .shadow_lg()
        .overflow_hidden()
        .text_size(SIZE.body)
        .text_color(t.foreground)
        .on_size_change((size, cx) => {
          if (Math.abs(size.width - this.width) > 1) {
            this.width = size.width;
            cx.notify();
          }
        });

    if (!v) return panel(null).child(this.row(t, "muted", div().text_color(t.muted).child("Controller")));
    if (v.dictating) return panel(380).child(this.dictating(t));
    if (v.mode === "picker" && v.picker) return panel(460).child(this.picker(t, v));
    if (v.mode === "menu" && v.menu) return panel(400).child(this.menu(t, v));
    if (v.mode === "legend" && v.legend) return panel(500).child(this.legend(t, v));
    if (v.mode === "layer" && v.layer) return panel(380).child(this.layer(t, v));
    if (v.hold) return panel(420).child(this.holding(t, v));
    return panel(null).child(this.compact(t, v));
  }

  // ---- pieces ----

  icon(t, color, size = 22) {
    return svg("mods/controller/pad.svg").w(size).h(size).flex_none().text_color(t[color] ?? color);
  }

  row(t, color, content) {
    return h_flex().px(16).py(12).gap(12).items_center().child(this.icon(t, color)).child(content);
  }

  key(t, name, dim = false) {
    const face = FACE[name];
    return div()
      .px(8)
      .py(3)
      .min_w(30)
      .rounded(6)
      .border(1)
      .border_color(face ?? t.input)
      .bg(face ?? t.surface)
      .text_size(SIZE.key)
      .font_semibold()
      .text_center()
      .text_color(face ? "#101010" : t.foreground)
      .when(dim, (el) => el.opacity(0.5))
      .child(name);
  }

  /** Where X goes next: the agents that need the person. */
  queue() {
    const list = agents();
    const blocked = list.filter((a) => a.status === "blocked");
    const done = list.filter((a) => a.status === "done");
    const first = blocked[0] ?? done[0];
    if (!first) return null;
    const n = blocked.length + done.length;
    const what = first.status === "blocked" ? "needs you" : "finished";
    return {
      first,
      count: n,
      blocked: blocked.length,
      text: `X → ${first.space_name} / ${first.tab_name} · ${what}${n > 1 ? ` (+${n - 1})` : ""}`,
    };
  }

  /** "space / tab" of what the controller acts on now. */
  here() {
    const f = focused();
    if (!f) return "";
    const s = spaces().find((s) => s.id === f.space);
    const tab = s?.tabs.find((x) => x.id === f.tab);
    return s ? `${s.name} / ${tab?.name ?? ""}` : "";
  }

  compact(t, v) {
    const q = this.queue();
    const showLabel = this.labelVisible && v.label;
    let line;
    let color = q?.blocked ? "blocked" : "accent";
    if (showLabel) {
      if (v.kind === "warn") color = "blocked";
      line = v_flex()
        .gap(2)
        .child(div().max_w(520).truncate().text_color(v.kind === "warn" ? t.blocked : t.foreground).child(v.label))
        .when(Boolean(v.hint), (el) => el.child(div().text_size(SIZE.small).text_color(t.muted).child(v.hint)));
    } else if (q) {
      line = div().max_w(520).truncate().child(q.text);
    } else {
      line = div().text_color(t.muted).child("Press View for buttons");
    }
    return this.row(t, color, line);
  }

  holding(t, v) {
    const inner = Math.max(0, (this.width || 420) - 32);
    return v_flex()
      .px(16)
      .py(14)
      .gap(10)
      .child(h_flex().gap(12).items_center().child(this.icon(t, "blocked")).child(div().font_semibold().child(v.hold.label)))
      .child(
        div()
          .h(6)
          .w_full()
          .rounded(3)
          .bg(t.surface)
          .child(div().h(6).rounded(3).bg(t.blocked).w(this.holdFull ? inner : 0).transition("width", v.hold.ms)),
      )
      .child(div().text_size(SIZE.small).text_color(t.muted).child("Let go to cancel"));
  }

  header(t, title, hint) {
    return h_flex()
      .px(16)
      .pt(14)
      .pb(10)
      .gap(12)
      .items_center()
      .child(this.icon(t, "accent"))
      .child(div().flex_1().truncate().text_size(SIZE.title).font_semibold().child(title))
      .when(Boolean(hint), (el) => el.child(div().text_size(SIZE.small).text_color(t.muted).child(hint)));
  }

  footer(t, parts) {
    return h_flex()
      .px(16)
      .py(12)
      .gap(18)
      .border_t(1)
      .border_color(t.border)
      .bg(t.chrome)
      .children(parts.map(([key, what], i) =>
        h_flex().id(`f${i}`).gap(8).items_center()
          .when(Boolean(key), (el) => el.child(this.key(t, key)))
          .child(div().text_size(SIZE.small).text_color(t.muted).child(what))));
  }

  menu(t, v) {
    const { items, index } = v.menu;
    const sel = items[index];
    const firstConfirm = items.findIndex((it) => it.confirm);
    const here = this.here();
    const tint = t.accent.length === 7 ? `${t.accent}40` : t.selected;
    return v_flex()
      .child(this.header(t, here ? `Menu · ${here}` : "Menu", null))
      .child(
        v_flex().px(8).pb(8).children(items.map((it, i) => {
          const on = i === index;
          return v_flex()
            .id(`m${i}`)
            .when(i === firstConfirm && i > 0, (el) => el.child(div().h(1).mx(8).my(6).bg(t.border)))
            .child(
              h_flex()
                .min_h(44)
                .px(12)
                .rounded(8)
                .gap(10)
                .items_center()
                .when(on, (el) => el.bg(tint))
                .child(div().w(4).h(24).rounded(2).bg(on ? t.accent : "#00000000"))
                .child(
                  div()
                    .flex_1()
                    .when(on, (el) => el.font_semibold())
                    .text_color(it.confirm ? t.blocked : t.foreground)
                    .child(it.label),
                ),
            );
        })),
      )
      .child(
        sel?.confirm
          ? this.footer(t, [["A", `Hold to ${sel.label.toLowerCase()}`], ["B", "close"]])
          : this.footer(t, [["D-pad", "move"], ["A", "pick"], ["B", "close"]]),
      );
  }

  /** New space…: a scrolling window of folders around the selection. */
  picker(t, v) {
    const { items, index, agent } = v.picker;
    const ROWS = 9;
    const start = Math.max(0, Math.min(index - Math.floor(ROWS / 2), items.length - ROWS));
    const shown = items.slice(start, start + ROWS);
    const tint = t.accent.length === 7 ? `${t.accent}40` : t.selected;
    const more = (n, where) => div().px(20).py(2).text_size(SIZE.small).text_color(t.dim).child(`${n} more ${where}`);
    return v_flex()
      .child(this.header(t, "New space", `${index + 1} of ${items.length}`))
      .child(div().px(16).pb(8).text_size(SIZE.small).text_color(t.muted).child(agent ? `Pick a folder. A starts ${agent} there, Y just a shell.` : "Pick a folder for it"))
      .when(start > 0, (el) => el.child(more(start, "above")))
      .child(
        v_flex().px(8).children(shown.map((f, k) => {
          const on = start + k === index;
          return h_flex()
            .id(`p${start + k}`)
            .min_h(44)
            .px(12)
            .rounded(8)
            .gap(10)
            .items_center()
            .when(on, (el) => el.bg(tint))
            .child(div().w(4).h(24).rounded(2).bg(on ? t.accent : "#00000000"))
            .child(div().flex_1().truncate().when(on, (el) => el.font_semibold()).child(f.name))
            .child(div().max_w(180).truncate().text_size(SIZE.small).text_color(t.dim).child(f.detail));
        })),
      )
      .when(start + ROWS < items.length, (el) => el.child(more(items.length - start - ROWS, "below")))
      .child(div().h(8))
      .child(this.footer(t, agent
        ? [["A", `+ ${agent}`], ["Y", "shell"], ["LB / RB", "page"], ["B", "back"]]
        : [["A", "create"], ["LB / RB", "page"], ["B", "back"]]));
  }

  legend(t, v) {
    const q = this.queue();
    const status = q ? `${q.count} agent${q.count === 1 ? "" : "s"} waiting on you · press X` : "Nothing waiting on you";
    return v_flex()
      .child(this.header(t, "Controller buttons", null))
      .child(div().px(16).pb(10).text_size(SIZE.small).text_color(q?.blocked ? t.blocked : t.muted).child(status))
      .child(
        v_flex().px(16).pb(12).gap(8).children(v.legend.map((r, i) =>
          h_flex().id(`l${i}`).gap(12).items_center()
            .child(h_flex().w(104).flex_none().child(this.key(t, r.button, r.dim)))
            .child(div().flex_1().text_color(r.dim ? t.dim : t.foreground).child(r.label)))),
      )
      .child(this.footer(t, [["View", "close"], ["Menu", "more actions"]]));
  }

  layer(t, v) {
    const list = spaces();
    return v_flex()
      .child(this.header(t, "Spaces", `Release ${v.layer.button} to close`))
      .child(
        v_flex().px(8).pb(8).children(list.map((s, i) => {
          const note = s.status === "blocked" ? "needs you" : s.status === "done" ? "finished" : s.status === "working" ? "working" : "";
          return h_flex()
            .id(`s${i}`)
            .min_h(44)
            .px(12)
            .rounded(8)
            .gap(10)
            .items_center()
            .when(s.active, (el) => el.bg(t.accent.length === 7 ? `${t.accent}40` : t.selected))
            .child(div().w(4).h(24).rounded(2).bg(s.active ? t.accent : "#00000000"))
            .child(div().flex_1().truncate().when(s.active, (el) => el.font_semibold()).text_color(s.active ? t.foreground : t.muted).child(s.name))
            .when(Boolean(note), (el) => el.child(div().text_size(SIZE.small).text_color(t[s.status] ?? t.muted).child(note)));
        })),
      )
      .child(this.footer(t, v.layer.legend.map((r) => [r.button, r.label.toLowerCase()])));
  }

  dictating(t) {
    return h_flex()
      .px(16)
      .py(16)
      .gap(12)
      .items_center()
      .child(div().size(14).rounded(7).bg(t.blocked).loop("opacity", { from: 0.35, to: 1, period: 900 }))
      .child(div().flex_1().text_size(SIZE.title).font_semibold().child("Listening…"))
      .child(div().text_size(SIZE.small).text_color(t.muted).child("Let go of RT to stop"));
  }
}
