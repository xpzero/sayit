// 通用终端判定：前台 App 的进程树宿主 TTY = 键盘直达（由 focus/mod.rs 的兜底调用）。
// 终端模拟器的定义性特征不是名字，而是"进程树里挂着带控制终端的 shell"——
// Terminal/iTerm2/wezterm/kitty/Warp……全满足，无需枚举名单（实测 wezterm 的 AX
// 只报 AXWindow，探不出输入框，但 ⌘V/键入必然落进终端）。附带效应：带集成终端的
// IDE（VS Code 等）同样命中——对它们注入通常正是想要的（编辑器接受键入）。

/// 前台 App 是否宿主命令行（probe 在 Unknown 时调用，一次 ps ~20-30ms）
pub fn hosts_tty(pid: i32) -> bool {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,tty="])
        .output()
    else {
        return false;
    };
    tty_in_subtree(&String::from_utf8_lossy(&out.stdout), pid)
}

/// 纯核心：ps 三列输出（pid/ppid/tty）里，root 的进程子树内是否有成员带 TTY
/// （tty 列非 "??" 开头）。BFS + visited 防御异常父指针环，节点数封顶 512。
fn tty_in_subtree(ps_output: &str, root: i32) -> bool {
    let mut children: std::collections::HashMap<i32, Vec<i32>> = Default::default();
    let mut tty_owner: std::collections::HashSet<i32> = Default::default();
    for line in ps_output.lines() {
        let mut it = line.split_whitespace();
        let (Some(p), Some(pp), Some(t)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(p), Ok(pp)) = (p.parse::<i32>(), pp.parse::<i32>()) else {
            continue;
        };
        children.entry(pp).or_default().push(p);
        if !t.starts_with('?') {
            tty_owner.insert(p);
        }
    }
    let mut queue = std::collections::VecDeque::from([root]);
    let mut visited = std::collections::HashSet::from([root]);
    let mut budget = 512; // 防御：异常大子树/环不该拖垮探测
    while let Some(p) = queue.pop_front() {
        if budget == 0 {
            return false;
        }
        budget -= 1;
        if tty_owner.contains(&p) {
            return true;
        }
        for &c in children.get(&p).into_iter().flatten() {
            if visited.insert(c) {
                queue.push_back(c);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 终端判定_子树宿主tty即命中_与App名字无关() {
        // 缩样：GUI 进程 51639（无 tty）→ shell 60001（ttys005）；陌生终端同样命中
        let ps = "  100     1 ??\n51639     1 ??\n60001 51639 ttys005\n60002 60001 ttys005\n";
        assert!(tty_in_subtree(ps, 51639), "子树里有 shell 带 TTY");
        assert!(tty_in_subtree(ps, 60001), "自身带 TTY 也算");
        // 纯 GUI 树（Finder 等）：无任何 TTY
        let gui = "  200     1 ??\n  201   200 ??\n  202   201 ??\n";
        assert!(!tty_in_subtree(gui, 200));
        // 根不存在 / 解析不了的行：安全侧 false
        assert!(!tty_in_subtree(gui, 999));
        assert!(!tty_in_subtree("", 51639));
        assert!(!tty_in_subtree("garbage line", 51639));
        // 异常环（a↔b 互为父）不陷入死循环
        let ring = "10 11 ??\n11 10 ??\n";
        assert!(!tty_in_subtree(ring, 10));
    }

    #[test]
    fn 终端判定_预算封顶与脏输入防御() {
        // 600 长链、TTY 挂在最末端：预算 512 耗尽 → 安全侧 false（不挂起、不误判终端）
        let mut chain = String::new();
        for i in 0..600 {
            chain.push_str(&format!("{} {} ??\n", 10_000 + i, 9_999 + i));
        }
        chain.push_str("10600 10599 ttys001\n");
        assert!(!tty_in_subtree(&chain, 9_999), "超预算子树按非终端处理");
        // 单问号（无 tty 的另一形态）不算 TTY
        assert!(!tty_in_subtree("5 4 ?\n", 4));
        // 表头行（非数字）被跳过，不把解析打崩
        let with_header = "  PID  PPID  TTY\n7 1 ??\n8 7 ttys009\n";
        assert!(tty_in_subtree(with_header, 7));
        // 负 pid 可解析、不 panic，行为与正 pid 一致
        assert!(tty_in_subtree("-3 -2 ttys001\n", -2));
        assert!(!tty_in_subtree("-3 -2 ??\n", -2));
    }
}
