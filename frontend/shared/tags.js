// Shared tag UI primitives — SolidJS hyperscript components used by both the
// panel and the mudra-keys status bar. Rendering only: every mutation goes
// through a callback prop, so the same component serves different hosts.
// Requires window.MudraSolid (solid-bundle.js) loaded first.
(function () {
  "use strict";
  const { h, For } = window.MudraSolid;

  // Capsule: the path-segment string of one tag. props: {tag:{id,path}, onSeg(tag,i,el), onRemove(tag), onAddChild(tag)}
  // Each segment is one path level; clicking a segment -> onSeg (host opens a same-level menu); leading x removes, trailing + adds a child.
  function Capsule(props) {
    const t = () => props.tag;
    const segs = () => t().path.split("::");
    return h("span.capsule", { onClick: (e) => e.stopPropagation() }, [
      props.onRemove ? h("span.cp-x", {
        title: "Delete this tag",
        onClick: () => props.onRemove(t()),
      }, "✕") : null,
      h(For, { each: segs() }, (seg, i) =>
        h("span", {
          class: () => "seg" + (i() === segs().length - 1 ? " leaf" : ""),
          onClick: (e) => props.onSeg && props.onSeg(t(), i(), e.currentTarget),
        }, seg)),
      props.onAddChild ? h("span.cp-close", {
        title: "Add child",
        onClick: () => props.onAddChild(t()),
      }, "＋") : null,
    ]);
  }

  // Filter chip: props {tag:{id,path}, on(t), class(on) prefix string}. The on state comes from the host.
  function Chip(props) {
    return h("button", {
      class: () => "chip" + (props.on() ? " on" : ""),
      onClick: () => props.onClick(),
    }, () => props.tag.path);
  }

  // Rank axis: props {root:{name,alias,rank_axis,children}, sel(child|undefined), onPick(child,k)}
  function RankAxis(props) {
    const sel = () => props.sel;
    return h("span.rank", {
      title: () => props.root.name + (props.root.alias ? "（" + props.root.alias + "）" : ""),
    }, [1, 2, 3, 4, 5].map((k) =>
      h("span", {
        class: () => "rk" + (k <= (sel() ? sel().rank : 0) ? " on" : ""),
        onClick: () => props.onPick && props.onPick(sel(), k),
      }, props.root.rank_axis)));
  }

  window.MudraTags = { Capsule, Chip, RankAxis };
})();
