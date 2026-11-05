use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;

slint::slint! {
    export { PointerBridge } from "../ui/selectable-text.slint";
    import { MemoryColumns, MemorySummary, MemoryItem, PageHeading } from "../ui/dashboard.slint";
    import { MemoryFooter } from "../ui/memory-footer.slint";
    import { MemoryRow, SecretBadge } from "../ui/common.slint";
    import { AppHeader } from "../ui/app-header.slint";
    import { RecallPage, RecallColumns } from "../ui/recall.slint";
    import { RecallRow, RecallStage, RecallOverview } from "../ui/recall-models.slint";
    import { RecallResult } from "../ui/recall-card.slint";
    import { SecretDialog, SecretContent } from "../ui/secret-dialog.slint";
    export component LayoutHarness inherits Window {
        min-width: 0px; max-width: 100000px;
        min-height: 0px; max-height: 100000px;
        preferred-width: 1160px; preferred-height: 820px;
        in property <bool> wide;
        in property <bool> compact;
        in property <bool> admin;
        in property <int> memory-order;
        in property <[MemoryRow]> memories;
        in property <MemoryRow> memory-card;
        out property <length> memory-card-height: memory-probe.height;
        out property <bool> memory-card-open: memory-probe.details-open;
        callback toggle-memory-card;
        toggle-memory-card => { memory-probe.toggle-details(); }
        out property <length> footer-right: footer-probe.details-right;
        out property <length> footer-status-right: footer-probe.status-right;
        out property <length> footer-height: footer-probe.height;
        out property <length> footer-agent-left: footer-probe.agent-left;
        out property <length> footer-status-top: footer-probe.status-top;
        out property <length> footer-details-top: footer-probe.details-top;
        in property <bool> recall-page;
        in property <bool> recall-done;
        in property <[RecallRow]> recall-results;
        in property <RecallOverview> overview;
        in property <[RecallStage]> stages;
        in property <RecallRow> card;
        out property <bool> card-open: card-probe.details-open;
        callback toggle-card;
        toggle-card => { card-probe.toggle-details(); }
        in property <bool> secret-revealed;
        in property <bool> secret-verified;
        in property <string> secret-label: "【演示】虚构的长名称测试服务凭据";
        in property <string> secret-error;
        in property <string> secret-value: "仅用于布局验证的虚构原文";
        out property <length> secret-scroll-height: secret-content-probe.content-height;
        out property <length> secret-panel-width: dialog-probe.panel-width;
        out property <length> secret-panel-height: dialog-probe.panel-height;
        out property <length> secret-action-width: dialog-probe.action-width;
        out property <length> secret-action-top: dialog-probe.action-top;
        in property <string> heading-title;
        out property <length> heading-height: heading-probe.height;
        out property <length> summary-height: summary-probe.height;
        out property <length> header-height: header-probe.height;
        out property <length> header-nav-right: header-probe.navigation-right;
        out property <length> header-exit-left: header-probe.exit-left;
        out property <length> badge-icon-center: badge-probe.icon-center-y;
        out property <length> badge-label-center: badge-probe.label-center-y;
        in property <bool> debug-open;
        in property <string> debug-meta: "HTTP 200 · 请求往返耗时 37 ms";
        out property <length> memory-column-width: columns.memory-column-width;
        out property <length> recall-width: recall.width;
        out property <length> content-height: recall.content-height;
        out property <length> result-width: result-probe.result-width;
        out property <length> card-height: card-probe.content-height;
        columns := MemoryColumns {
            width: parent.width;
            visible: !root.recall-page;
            wide: root.wide;
            compact: root.compact;
            order: root.memory-order;
            memories: root.memories;
        }
        recall := RecallPage {
            width: parent.width;
            visible: root.recall-page;
            compact: root.compact;
            recall-done: root.recall-done;
            recall-results: root.recall-results;
            overview: root.overview; stages: root.stages;
            debug-open: root.debug-open;
            debug-meta: root.debug-meta;
            debug-request: "{\"query\":\"虚构查询\"}";
            debug-response: "{\"memories\":[]}";
        }
        result-probe := RecallColumns { width: parent.width; visible: false; wide: root.wide; results: root.recall-results; overview: root.overview; stages: root.stages; }
        card-probe := RecallResult { width: parent.width; visible: false; memory: root.card; }
        memory-probe := MemoryItem { width: parent.width; visible: false; memory: root.memory-card; compact: root.compact; }
        footer-probe := MemoryFooter { width: parent.width; visible: false; agent-name: "虚构验收 Agent"; usage: "召回 123 · 有用 8 / 已评 12"; }
        secret-content-probe := SecretContent { width: parent.width; height: 180px; visible: false; value: root.secret-value; }
        dialog-probe := SecretDialog {
            width: parent.width; height: parent.height; visible: false;
            label: root.secret-label;
            verified: root.secret-verified;
            error: root.secret-error;
            revealed: root.secret-revealed; value: root.secret-value;
        }
        heading-probe := PageHeading {
            width: parent.width; visible: false; compact: root.compact;
            refresh-visible: true; title: root.heading-title; subtitle: "记忆概览 / demo";
        }
        summary-probe := MemorySummary {
            width: parent.width; visible: false; compact: root.compact;
            memory-count: "10"; event-count: "13"; pending-count: "0"; secret-summary: "含 1 项 Secret";
        }
        header-probe := AppHeader {
            width: parent.width; visible: false; unlocked: true;
            compact: parent.width < 1040px; page-padding: 20px; recall-page: root.recall-page;
            show-brand: parent.width >= 420px;
            can-manage: root.admin; show-icon: !(root.admin && parent.width < 380px); dense: parent.width < 340px;
        }
        badge-probe := SecretBadge { visible: false; }
    }
}

struct LayoutPlatform {
    window: Rc<slint::platform::software_renderer::MinimalSoftwareWindow>,
}

impl slint::platform::Platform for LayoutPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
}

#[test]
fn memory_column_uses_available_width_in_real_slint_layout() {
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(LayoutPlatform {
        window: window.clone(),
    }))
    .unwrap();
    let ui = LayoutHarness::new().unwrap();
    ui.show().unwrap();
    let pointer_updates = Rc::new(std::cell::RefCell::new(Vec::new()));
    let updates = pointer_updates.clone();
    ui.global::<PointerBridge>()
        .on_update(move |arrow| updates.borrow_mut().push(arrow));
    let previous = ui.global::<PointerBridge>().invoke_enter(false);
    let current = ui.global::<PointerBridge>().invoke_enter(true);
    ui.global::<PointerBridge>().invoke_leave(previous);
    assert_eq!(
        *pointer_updates.borrow(),
        [false, true],
        "旧区域迟到的退出不能清除新区域的指针模式"
    );
    ui.global::<PointerBridge>().invoke_leave(current);
    assert_eq!(*pointer_updates.borrow(), [false, true, false]);
    let settle_layout = || {
        let size = ui.window().size();
        let mut pixels = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size.width, size.height);
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(pixels.make_mut_slice(), size.width as usize);
        });
    };
    for has_memories in [false, true] {
        ui.set_memories(if has_memories {
            ModelRc::new(VecModel::from(vec![
                MemoryRow {
                    content: "仅用于测试的虚构记忆。".into(),
                    agent_name: "虚构验收 Agent".into(),
                    usage: "召回 123 · 有用 8 / 已评 12".into(),
                    ..Default::default()
                },
                MemoryRow {
                    content: "【演示】虚构凭据说明".into(),
                    agent_name: "虚构验收 Agent".into(),
                    usage: "召回 2 · 未评价".into(),
                    scope: "项目 · bronze-trail-demo".into(),
                    is_secret: true,
                    secret_stored: true,
                    secret_status: "密文已保存".into(),
                    ..Default::default()
                },
            ]))
        } else {
            ModelRc::new(VecModel::<MemoryRow>::default())
        });
        for width in [1240.0_f32, 1040.0, 900.0, 680.0, 390.0, 240.0] {
            let padding = if width < 680.0 {
                20.0
            } else {
                36.0_f32.max((width - 1160.0) / 2.0)
            };
            let available = width - 2.0 * padding;
            ui.window()
                .set_size(slint::LogicalSize::new(available, 820.0));
            ui.set_wide(width >= 1040.0);
            ui.set_compact(width < 680.0);
            settle_layout();
            assert!(
                (ui.get_footer_right() - available).abs() < 1.0,
                "详情按钮须贴齐内容右边缘"
            );
            assert!(
                ui.get_footer_agent_left().abs() < 1.0,
                "Agent 来源须贴齐正文左边缘"
            );
            assert!(
                ui.get_footer_status_top().abs() < 1.0 && ui.get_footer_details_top().abs() < 1.0,
                "窄屏状态与详情须对齐来源行，不能悬在两行之间"
            );
            assert!(
                (ui.get_footer_right() - ui.get_footer_status_right() - 48.0).abs() < 1.0,
                "同步状态须紧邻详情，不能悬在中间"
            );
            assert_eq!(
                ui.get_footer_height(),
                if available < 520.0 { 44.0 } else { 24.0 }
            );
            let note = MemoryRow {
                memory_id: format!("fixture-{width}").into(),
                content: "虚构短记忆。".into(),
                detail: "修订 1 · 0 条来源".into(),
                agent_name: "虚构验收 Agent".into(),
                usage: "召回 0".into(),
                ..Default::default()
            };
            ui.set_memory_card(note.clone());
            settle_layout();
            let short_card = ui.get_memory_card_height();
            ui.invoke_toggle_memory_card();
            settle_layout();
            assert!(ui.get_memory_card_open() && ui.get_memory_card_height() > short_card + 40.0);
            let mut revised = note.clone();
            revised.detail = "修订 2 · 0 条来源".into();
            ui.set_memory_card(revised);
            settle_layout();
            assert!(
                !ui.get_memory_card_open(),
                "更正后不能保留旧修订的详情展开状态"
            );
            let mut excerpted = note.clone();
            excerpted.memory_id = format!("excerpt-{width}").into();
            excerpted.preview = "虚构短记忆。…".into();
            excerpted.content = format!(
                "{}【完整正文末尾】",
                "跨环境验证结果须保留上下文。".repeat(80)
            )
            .into();
            ui.set_memory_card(excerpted);
            settle_layout();
            let collapsed = ui.get_memory_card_height();
            ui.invoke_toggle_memory_card();
            settle_layout();
            assert!(
                ui.get_memory_card_height() > collapsed + 200.0,
                "详情必须展开完整正文，而非继续显示服务端短预览"
            );
            ui.invoke_toggle_memory_card();
            settle_layout();
            assert!(
                (ui.get_memory_card_height() - collapsed).abs() < 1.0,
                "收起后恢复短预览，不能保留全文的空白高度"
            );
            let mut long_note = note;
            long_note.content = "用于验证正文完整布局的虚构长记忆。".repeat(20).into();
            ui.set_memory_card(long_note);
            settle_layout();
            assert!(
                ui.get_memory_card_height() > short_card + 30.0,
                "卡片须随正文增高，避免裁切或固定留白"
            );
            let memory = ui.get_memory_column_width();
            let expected = available - if width >= 1040.0 { 304.0 } else { 0.0 };
            for order in [0, 1, 2] {
                ui.set_memory_order(order);
                settle_layout();
                assert!(
                    (ui.get_memory_column_width() - expected).abs() <= 2.0,
                    "统计视图不能挤压记忆列"
                );
            }
            ui.set_memory_order(0);
            assert!(
                (memory - expected).abs() <= 2.0,
                "窗口 {width}、有记忆 {has_memories}：内容宽 {available}，记忆列 {memory}，预期 {expected}"
            );
            assert!(memory >= if width >= 1040.0 { 600.0 } else { width - 74.0 });
            assert!(
                ui.get_summary_height() <= if width < 680.0 { 120.0 } else { 140.0 },
                "统计摘要不能挤占手机首屏，窗口 {width}"
            );
            ui.set_heading_title("短标题".into());
            settle_layout();
            let short_heading = ui.get_heading_height();
            ui.set_heading_title("这是用于验证标题换行的虚构空间名称".repeat(3).into());
            settle_layout();
            assert!(
                ui.get_heading_height() > short_heading + 15.0,
                "长标题应增加行数与标题区高度，窗口 {width}"
            );
        }
    }
    ui.set_recall_page(true);
    ui.set_recall_done(true);
    for width in [1240.0_f32, 1040.0, 900.0, 680.0, 390.0, 240.0] {
        let padding = if width < 680.0 {
            20.0
        } else {
            36.0_f32.max((width - 1160.0) / 2.0)
        };
        let available = width - 2.0 * padding;
        ui.window()
            .set_size(slint::LogicalSize::new(available, 820.0));
        ui.set_wide(width >= 1040.0);
        ui.set_compact(width < 680.0);
        let model = |content: String| {
            ModelRc::new(VecModel::from(vec![RecallRow {
                content: content.into(),
                scope: "虚构项目".into(),
                ..Default::default()
            }]))
        };
        ui.set_overview(RecallOverview {
            has_diagnostics: true,
            index_available: true,
            total_time: "24 ms".into(),
            client_time: "37 ms".into(),
            ..Default::default()
        });
        ui.set_stages(ModelRc::new(VecModel::from(
            (1..=4)
                .map(|number| RecallStage {
                    number: format!("0{number}").into(),
                    title: "虚构检索阶段".into(),
                    value: "3".into(),
                    note: "仅用于布局验证的虚构数量说明。".into(),
                    warning: false,
                })
                .collect::<Vec<_>>(),
        )));
        ui.set_recall_results(model("虚构的短召回正文".into()));
        settle_layout();
        let short_height = ui.get_content_height();
        ui.set_recall_results(model("这是一段仅用于布局回归的虚构长记忆。".repeat(200)));
        settle_layout();
        let long_height = ui.get_content_height();
        assert!(
            long_height > short_height + 100.0,
            "窗口 {width}：短结果高 {short_height}，长结果高 {long_height}，应随正文增长"
        );
        assert!((ui.get_recall_width() - available).abs() <= 2.0);
        let expected_result = available - if width >= 1040.0 { 344.0 } else { 0.0 };
        assert!(
            (ui.get_result_width() - expected_result).abs() <= 2.0,
            "召回正文列应占可用空间，窗口 {width}"
        );
        ui.set_card(RecallRow {
            view_key: format!("fixture-{width}").into(),
            content: "虚构记忆正文".into(),
            memory_id: "01900000-0000-7000-8000-000000000000".into(),
            detail: "修订 1 · 2 条来源".into(),
            path: "Qdrant BM25".into(),
            score: "BM25 分数 2.5".into(),
            ..Default::default()
        });
        settle_layout();
        assert!(!ui.get_card_open());
        let closed_card = ui.get_card_height();
        ui.invoke_toggle_card();
        settle_layout();
        assert!(ui.get_card_open());
        assert!(
            ui.get_card_height() > closed_card + 100.0,
            "展开元数据需要增加高度，窗口 {width}"
        );
        ui.set_card(RecallRow {
            view_key: format!("secret-{width}").into(),
            is_secret: true,
            secret_stored: true,
            secret_status: "密文已保存".into(),
            secret_ref: "secret://long-demo-space/01900000-0000-7000-8000-000000000000".into(),
            content: "【演示】已保存的虚构凭据说明".into(),
            ..Default::default()
        });
        settle_layout();
        assert!(!ui.get_card_open());
        assert!(
            ui.get_card_height() > closed_card + 100.0,
            "Secret 状态与引用应在未展开详情时占据实际布局，窗口 {width}"
        );
        ui.set_card(RecallRow {
            view_key: format!("replacement-{width}").into(),
            content: "另一次查询的虚构记忆".into(),
            ..Default::default()
        });
        settle_layout();
        assert!(!ui.get_card_open(), "切换查询对象不继承上次展开状态");
        ui.set_debug_open(true);
        ui.set_debug_meta("HTTP 200 · 请求往返耗时 37 ms".into());
        settle_layout();
        let expanded_height = ui.get_content_height();
        assert!(
            ui.get_content_height() > long_height + 500.0,
            "窗口 {width}：展开调试区后应给 JSON 输入框留足高度"
        );
        ui.set_debug_meta(
            "虚构记忆 01900000-0000-7000-8000-000000000000 · Qdrant BM25 · 2.500000 · 索引修订 1\n"
                .repeat(10)
                .into(),
        );
        settle_layout();
        assert!(
            ui.get_content_height() > expanded_height + 100.0,
            "长诊断概览应换行并增加高度，窗口 {width}"
        );
        ui.set_debug_open(false);
    }
    ui.set_secret_label(
        "虚构 MySQL、ClickHouse 连接凭据；长标题布局回归验证。"
            .repeat(3)
            .into(),
    );
    ui.set_secret_value("虚构多行原文不应被裁切。\n".repeat(90).into());
    for width in [240.0_f32, 390.0, 680.0, 1240.0] {
        for height in [160.0_f32, 320.0, 820.0] {
            for (verified, revealed) in [(false, false), (true, false), (true, true)] {
                ui.window().set_size(slint::LogicalSize::new(width, height));
                ui.set_secret_revealed(revealed);
                assert!(
                    ui.get_secret_scroll_height() > 1000.0,
                    "长原文必须撑开滚动内容，而非固定高度裁切"
                );
                ui.set_secret_verified(verified);
                settle_layout();
                assert!(
                    ui.get_secret_panel_width() > 0.0
                        && ui.get_secret_panel_width() <= width - 30.0,
                    "Secret 浮层不能横向溢出：{width}×{height}"
                );
                assert!(
                    ui.get_secret_panel_height() > 0.0
                        && ui.get_secret_panel_height() <= height - 30.0,
                    "Secret 浮层不能纵向溢出：{width}×{height}"
                );
                let inset = if width < 360.0 { 36.0 } else { 56.0 };
                assert!(
                    (ui.get_secret_action_width() - (ui.get_secret_panel_width() - inset)).abs()
                        <= 18.0,
                    "Secret 操作按钮应撑满内容宽度：{width}×{height}"
                );
                let panel_height = ui.get_secret_panel_height();
                let action_top = ui.get_secret_action_top();
                ui.set_secret_error("空间密码不正确，请重试。".into());
                settle_layout();
                assert!(
                    (ui.get_secret_panel_height() - panel_height).abs() < 1.0
                        && (ui.get_secret_action_top() - action_top).abs() < 1.0,
                    "验密失败不应推动浮层与主按钮：{width}×{height}"
                );
                ui.set_secret_error("".into());
            }
        }
    }
    for width in [240.0_f32, 320.0, 390.0, 768.0, 1397.0] {
        ui.window().set_size(slint::LogicalSize::new(width, 820.0));
        settle_layout();
        assert!(ui.get_header_height() <= 56.0, "页头应保持单行紧凑高度");
        assert!(
            ui.get_header_nav_right() <= ui.get_header_exit_left(),
            "导航与退出不得重叠，窗口 {width}"
        );
        assert!(
            (ui.get_badge_icon_center() - 12.0).abs() < 0.5
                && (ui.get_badge_label_center() - 12.0).abs() < 0.5,
            "Secret 锁图标与文字必须共同居中"
        );
    }
    ui.set_admin(true);
    for width in [240.0_f32, 320.0, 390.0, 768.0, 1397.0] {
        ui.window().set_size(slint::LogicalSize::new(width, 820.0));
        settle_layout();
        assert!(
            ui.get_header_nav_right() <= ui.get_header_exit_left(),
            "管理空间增加系统设置后导航仍不能重叠：{width}"
        );
    }
}
