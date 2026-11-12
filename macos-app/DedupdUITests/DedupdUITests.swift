import XCTest

/// Drives the app with a canned daemon behind it. Started with
/// `-uiTestFixture <name>` the app opens its main window on fixed data and
/// touches neither the real daemon nor the owner's preferences.
///
/// Each test attaches pictures of the window; `just app-screenshots` exports
/// them.
@MainActor
final class DedupdUITests: XCTestCase {
    private var app = XCUIApplication()

    override func setUp() async throws {
        continueAfterFailure = false
    }

    override func tearDown() async throws {
        app.terminate()
    }

    // MARK: - Helpers

    /// Start the app on a canned daemon and return its main window.
    private func launch(_ fixture: String, pane: String? = nil) -> XCUIElement {
        launch(arguments: ["-uiTestFixture", fixture] + (pane.map { ["-pane", $0] } ?? []))
    }

    private func launch(arguments: [String]) -> XCUIElement {
        app = XCUIApplication()
        app.launchArguments = arguments
        app.launch()
        app.activate()
        let window = app.windows.firstMatch
        XCTAssertTrue(window.waitForExistence(timeout: 10), "the window opens")
        return window
    }

    /// The element with this identifier, whatever kind it is.
    private func element(_ identifier: String, in window: XCUIElement, timeout: TimeInterval = 5) -> XCUIElement {
        let element = window.descendants(matching: .any)[identifier].firstMatch
        XCTAssertTrue(element.waitForExistence(timeout: timeout), "\(identifier) is on screen")
        return element
    }

    /// A piece of text on screen that has these words in it. On macOS a
    /// text's words are its accessibility value, not its label.
    private func text(_ words: String, in window: XCUIElement) -> XCUIElement {
        window.staticTexts.matching(NSPredicate(format: "value CONTAINS %@", words)).firstMatch
    }

    /// An element of any kind whose label has these words in it.
    private func labelled(_ words: String, in window: XCUIElement) -> XCUIElement {
        window.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", words)).firstMatch
    }

    private func go(to pane: String, in window: XCUIElement) {
        element("pane-\(pane)", in: window).click()
    }

    /// A picture of the window. It is cut out of the screen, so the app is
    /// brought to the front first; a window of another app that is in front
    /// of it would be in the picture.
    private func screenshot(_ window: XCUIElement, _ name: String) {
        app.activate()
        let attachment = XCTAttachment(screenshot: window.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    // MARK: - The screens

    func testEverySidebarRowOpensItsScreen() {
        let window = launch("running")
        for (pane, title) in [
            ("downloads", "Downloads"), ("storage", "Storage"), ("daemon", "Daemon"),
            ("discover", "Discover"), ("library", "Library"),
        ] {
            go(to: pane, in: window)
            XCTAssertTrue(window.title.hasPrefix(title), "the \(pane) row opens its screen, not \(window.title)")
        }
    }

    func testLibraryListsTheModelsAndShowsOneInFull() {
        let window = launch("running")

        let model = element("model-bartowski/SmolLM2-135M-Instruct-GGUF", in: window)
        XCTAssertTrue(
            element("model-mlx-community/SmolLM-135M-Instruct-4bit", in: window).exists,
            "a model no app has is listed too"
        )
        XCTAssertEqual(element("sidebar-status", in: window).label, "Running, version 0.1.0")
        screenshot(window, "01-library")

        model.click()
        XCTAssertTrue(element("remove-ollama", in: window).exists, "Ollama has the model, so it can be deactivated there")
        XCTAssertTrue(element("place-hf", in: window).exists, "the Hugging Face cache does not, so it can be activated there")
        XCTAssertTrue(labelled("Parameters: 135M", in: window).exists)
        screenshot(window, "02-library-inspector")

        element("row-density", in: window).click()
        XCTAssertEqual(
            element("model-bartowski/SmolLM2-135M-Instruct-GGUF", in: window).label,
            "bartowski/SmolLM2-135M-Instruct-GGUF, apache-2.0",
            "larger rows add a second line"
        )
        screenshot(window, "03-library-larger-rows")

        element("remove-ollama", in: window).click()
        XCTAssertTrue(element("confirm-removal", in: window).exists, "nothing is removed before the owner has seen the list")
        XCTAssertTrue(text("manifests/hf.co/bartowski/SmolLM2-135M-Instruct-GGUF/Q4_K_M", in: window).exists)
        screenshot(window, "04-remove-from-app")
    }

    func testAModelIsArchivedAndRestored() {
        let window = launch("running")
        element("model-lmstudio-community/Qwen3.8-27B-MLX-4bit", in: window).click()

        element("archive-model", in: window).click()
        XCTAssertTrue(element("confirm-archive", in: window).exists, "nothing is archived before the owner has seen what it does")
        XCTAssertTrue(element("archive-steps", in: window).label.contains("The model is taken out of Bionic."))
        screenshot(window, "05-archive-a-model")

        element("confirm-archive", in: window).click()
        XCTAssertTrue(element("restore-model", in: window).waitForExistence(timeout: 5), "an archived model can be restored")
        XCTAssertFalse(
            window.descendants(matching: .any)["remove-bionic"].firstMatch.exists,
            "an archived model is in no app"
        )
        screenshot(window, "06-archived-model")

        element("restore-model", in: window).click()
        XCTAssertTrue(element("archive-model", in: window).waitForExistence(timeout: 5), "a restored model can be archived again")
    }

    func testDiscoverFindsAModelAndQueuesADownload() {
        let window = launch("running", pane: "discover")

        let hit = element("hit-unsloth/Qwen3.8-9B-Instruct-GGUF", in: window)
        XCTAssertTrue(labelled("In Bionic and Ollama", in: window).exists, "a model that is already here says so")
        hit.click()

        let ollama = element("target-gguf-ollama", in: window)
        XCTAssertEqual(ollama.value as? Int, 1, "GGUF goes into Ollama unless the owner chose otherwise")
        XCTAssertEqual(element("target-gguf-hf", in: window).value as? Int, 0)
        screenshot(window, "05-discover")

        element("download-Q4_K_M", in: window).click()
        XCTAssertFalse(app.sheets.firstMatch.exists, "the daemon took the download")
        go(to: "downloads", in: window)
        // The canned daemon numbers what the app queues from 20.
        let queued = element("download-20", in: window).label
        XCTAssertTrue(queued.contains("unsloth/Qwen3.8-9B-Instruct-GGUF:Q4_K_M"), "the download is in the queue: \(queued)")
        screenshot(window, "06-download-queued")
    }

    func testDownloadsShowProgressAndCanBePaused() {
        let window = launch("downloading", pane: "downloads")

        let running = element("download-7", in: window)
        XCTAssertTrue(running.label.contains("41.94 MB of 106.07 MB"), "the progress of the download: \(running.label)")
        XCTAssertTrue(element("download-5", in: window).label.contains("accept the model's terms"), "why a download needs the owner")
        XCTAssertTrue(element("governor", in: window).exists)
        running.click()
        XCTAssertTrue(text("Found 4 files at revision", in: window).waitForExistence(timeout: 5), "the download's history")
        screenshot(window, "07-downloads")

        element("pause-7", in: window).click()
        XCTAssertTrue(element("resume-7", in: window).exists, "a paused download can be resumed")
        XCTAssertTrue(element("download-7", in: window).label.contains("Paused"))

        element("clear-finished", in: window).click()
        XCTAssertFalse(window.descendants(matching: .any)["download-4"].firstMatch.waitForExistence(timeout: 1), "the finished one is gone")
        screenshot(window, "08-downloads-paused")
    }

    func testStorageShowsThePlacesAndAsksBeforeCleaningUp() {
        let window = launch("running", pane: "storage")

        XCTAssertTrue(element("location-store", in: window).exists)
        XCTAssertTrue(element("location-archive", in: window).exists)
        XCTAssertTrue(element("sharing-saves", in: window).exists)
        XCTAssertTrue(element("check-result", in: window).exists)
        screenshot(window, "09-storage")

        element("review-clean", in: window).click()
        XCTAssertTrue(element("confirm-clean", in: window).exists, "the files are listed before anything is removed")
        screenshot(window, "10-clean-up")

        element("confirm-clean", in: window).click()
        XCTAssertTrue(text("The last clean-up freed 4.92 GB.", in: window).waitForExistence(timeout: 5))
    }

    func testDaemonScreenShowsTheServiceAndItsSettings() {
        let window = launch("running", pane: "daemon")

        XCTAssertTrue(text("Running, version 0.1.0", in: window).waitForExistence(timeout: 5))
        XCTAssertTrue(element("stop-agent", in: window).exists)
        XCTAssertTrue(element("uninstall-agent", in: window).exists)
        XCTAssertTrue(element("apply-settings", in: window).exists)
        XCTAssertFalse(element("apply-settings", in: window).isEnabled, "nothing to apply until something is changed")
        screenshot(window, "11-daemon")
    }

    func testWithoutADaemonTheAppOffersToInstallIt() {
        let window = launch("notRunning")

        XCTAssertTrue(text("The daemon is not running", in: window).waitForExistence(timeout: 5))
        XCTAssertEqual(element("sidebar-status", in: window).label, "The daemon is not running")
        screenshot(window, "12-not-running")

        element("go-to-daemon", in: window).click()
        XCTAssertTrue(element("install-agent", in: window).exists)
        screenshot(window, "13-install")
    }

    func testMenuBarPanelShowsTheStateAtAGlance() {
        let window = launch(arguments: ["-uiTestFixture", "downloading", "-panelWindow", "YES"])

        XCTAssertTrue(text("Running, version 0.1.0", in: window).waitForExistence(timeout: 5))
        XCTAssertEqual(element("tile-store", in: window).label, "Store, 290.04 GB, 61 files")
        XCTAssertEqual(element("tile-shared", in: window).label, "Shared with apps, 290.07 GB, not stored twice")
        XCTAssertEqual(element("tile-archive", in: window).label, "Archive, Attached, dedupd")
        XCTAssertTrue(labelled("Downloading, 41.94 MB of 106.07 MB", in: window).exists, "the download in flight")
        XCTAssertTrue(element("open-main", in: window).isEnabled)
        screenshot(window, "14-menu-panel")

        element("scan-button", in: window).click()
        XCTAssertTrue(element("scan-result", in: window, timeout: 5).exists)
    }

    func testPanelFieldOpensAModelByItsAddress() {
        let window = launch(arguments: ["-uiTestFixture", "running", "-panelWindow", "YES"])
        let field = element("panel-find", in: window)

        // An address that only looks like Hugging Face's goes nowhere.
        field.click()
        field.typeText("https://huggingface.co@evil.example/org/repo\n")
        XCTAssertTrue(text("That is not a model's name or an address on huggingface.co", in: window).waitForExistence(timeout: 5))
        XCTAssertEqual(app.windows.count, 1, "no other window opened")
        screenshot(window, "15-menu-panel-refused")

        // The model's own address opens Discover on that model.
        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeText("https://hf.co/bartowski/SmolLM2-135M-Instruct-GGUF?download=true\n")
        let shown = app.staticTexts.matching(
            NSPredicate(format: "value CONTAINS %@", "The inspector shows bartowski/SmolLM2-135M-Instruct-GGUF")
        ).firstMatch
        XCTAssertTrue(shown.waitForExistence(timeout: 10), "Discover shows the model")
        let main = app.windows.containing(.any, identifier: "pane-discover").firstMatch
        XCTAssertTrue(main.exists)
        screenshot(main, "16-discover-from-panel")
    }

    /// The real thing: the app, the Rust client and a running daemon. Skipped
    /// unless a daemon's address is given, because it downloads a model:
    ///
    ///     just app-live 127.0.0.1:8643
    ///
    /// Point it at a daemon with a scratch home, not at the real one.
    func testLiveDaemonDownloadsAModel() throws {
        let environment = ProcessInfo.processInfo.environment
        let address = (environment["DEDUPD_LIVE_ADDRESS"] ?? "").trimmingCharacters(in: .whitespaces)
        try XCTSkipIf(address.isEmpty, "set DEDUPD_LIVE_ADDRESS to run against a daemon")
        let model = environment["DEDUPD_LIVE_MODEL"] ?? "bartowski/SmolLM2-135M-Instruct-GGUF:Q4_K_M"
        let parts = model.split(separator: ":").map(String.init)
        let repository = parts[0]
        let variant = parts.count > 1 ? parts[1] : "all files"

        let window = launch(arguments: ["-mainWindow", "YES", "-daemonAddress", address, "-pane", "discover"])
        XCTAssertTrue(
            element("sidebar-status", in: window).label.contains("Running, version"),
            "the daemon at \(address) answers"
        )

        let search = window.searchFields.firstMatch
        XCTAssertTrue(search.waitForExistence(timeout: 10))
        search.click()
        search.typeText(repository + "\n")
        let download = element("download-\(variant)", in: window, timeout: 30)
        screenshot(window, "20-live-model")
        download.click()

        go(to: "downloads", in: window)
        let done = window.descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@ AND label CONTAINS 'Activated in'", model)).firstMatch
        XCTAssertTrue(done.waitForExistence(timeout: 300), "the download of \(model) finishes")
        screenshot(window, "21-live-downloaded")

        go(to: "library", in: window)
        XCTAssertTrue(element("model-\(repository)", in: window, timeout: 20).exists, "the model is in the library")
        screenshot(window, "22-live-library")
    }
}
