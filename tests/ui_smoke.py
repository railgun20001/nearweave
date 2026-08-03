from playwright.sync_api import sync_playwright


def main() -> None:
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 960})
        errors: list[str] = []
        page.on("console", lambda message: errors.append(message.text) if message.type == "error" else None)
        page.goto("http://127.0.0.1:1420/?mock")
        page.wait_for_load_state("networkidle")

        page.get_by_role("button", name="停止连接").wait_for()
        page.get_by_text("NearWeave 连接运行中 · 1 台已连接").wait_for()
        page.get_by_title("当前发送目标").wait_for()
        page.get_by_role("button", name="取消传输：产品演示.mp4").wait_for()
        page.get_by_role("button", name="设计素材").click()
        page.get_by_text("文件夹 · 点击后加载").wait_for()
        page.get_by_role("button", name="打开文件夹").click()
        page.get_by_text("NearWeave 图标.sketch").wait_for()
        page.get_by_title("下载文件").wait_for()

        page.get_by_role("button", name="打开设置").click()
        page.get_by_text("局域网传输", exact=True).wait_for()
        page.get_by_text("已启用", exact=True).wait_for()
        page.get_by_role("button", name="关闭设置").click()

        setup_page = browser.new_page(viewport={"width": 1180, "height": 760})
        setup_page.goto("http://127.0.0.1:1420/?mock&lan-setup")
        setup_page.wait_for_load_state("networkidle")
        setup_page.get_by_role("dialog", name="启用局域网传输？").wait_for()
        setup_page.get_by_role("button", name="暂不开启").wait_for()
        setup_page.get_by_role("button", name="启用局域网传输").wait_for()

        assert not errors, f"浏览器控制台出现错误：{errors}"
        browser.close()


if __name__ == "__main__":
    main()
