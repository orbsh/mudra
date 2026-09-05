// Shared tag UI primitives — SolidJS hyperscript components used by both the
// panel and the mudra-keys status bar. Rendering only: every mutation goes
// through a callback prop, so the same component serves different hosts.
// Requires window.MudraSolid (solid-bundle.js) loaded first.
(function () {
  "use strict";
  const { h, For } = window.MudraSolid;

  // 胶囊：一个 tag 的路径段串。props: {tag:{id,path}, onSeg(tag,i,el), onRemove(tag), onAddChild(tag)}
  // 每段一级路径，点击段 → onSeg（宿主弹同级菜单）；头✕删，尾＋加子级。
  function Capsule(props) {
    const t = () => props.tag;
    const segs = () => t().path.split("::");
    return h("span.capsule", { onClick: (e) => e.stopPropagation() }, [
      props.onRemove ? h("span.cp-x", {
        title: "删除此标签",
        onClick: () => props.onRemove(t()),
      }, "✕") : null,
      h(For, { each: segs() }, (seg, i) =>
        h("span", {
          class: () => "seg" + (i() === segs().length - 1 ? " leaf" : ""),
          onClick: (e) => props.onSeg && props.onSeg(t(), i(), e.currentTarget),
        }, seg)),
      props.onAddChild ? h("span.cp-close", {
        title: "添加子级",
        onClick: () => props.onAddChild(t()),
      }, "＋") : null,
    ]);
  }

  // 过滤 chip：props {tag:{id,path}, on(t), class(on) 前置字符串}。on 状态由宿主给。
  function Chip(props) {
    return h("button", {
      class: () => "chip" + (props.on() ? " on" : ""),
      onClick: () => props.onClick(),
    }, () => props.tag.path);
  }

  // 排名轴：props {root:{name,alias,rank_axis,children}, sel(child|undefined), onPick(child,k)}
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
