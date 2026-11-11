import XCTest

/// Drives the panel with a canned daemon behind it. The app is started with
/// `-uiTestFixture <name>`, which also shows the panel in an ordinary window:
/// a menu bar panel closes when it loses focus and cannot be driven reliably.
///
/// Each test attaches a picture of the panel; `just app-screenshots` exports
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

    private func launch(_ fixture: String) -> XCUIElement {
        launch(arguments: ["-uiTestFixture", fixture])
    }

    private func launch(arguments: [String]) -> XCUIElement {
        app = XCUIApplication()
        app.launchArguments = arguments
        app.launch()
        let window = app.windows["dedupd"]
        XCTAssertTrue(window.waitForExistence(timeout: 10), "the panel window opens")
        return window
    }

    /// What VoiceOver reads for the element with this identifier. The tiles
    /// and the progress bar are each one element, so their text is in the label.
    private func label(_ identifier: String, in window: XCUIElement) -> String {
        let element = window.descendants(matching: .any)[identifier].firstMatch
        XCTAssertTrue(element.waitForExistence(timeout: 5), "\(identifier) is on screen")
        return element.label
    }

    /// An element of any kind by its label, whatever its case: section
    /// headings are shown in capitals.
    private func anything(in window: XCUIElement, labelled label: String) -> XCUIElement {
        window.descendants(matching: .any).matching(NSPredicate(format: "label ==[c] %@", label)).firstMatch
    }

    /// An element of any kind whose label has this text in it: a row of the
    /// model list is one element that reads its name, its detail and its size.
    private func anything(in window: XCUIElement, labelContains text: String) -> XCUIElement {
        window.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", text)).firstMatch
    }

    /// A piece of text on screen that has these words in it. On macOS a text's
    /// words are its accessibility value, not its label.
    private func text(in window: XCUIElement, containing words: String) -> XCUIElement {
        window.staticTexts.matching(NSPredicate(format: "value CONTAINS %@", words)).firstMatch
    }

    private func screenshot(_ window: XCUIElement, _ name: String) {
        let attachment = XCTAttachment(screenshot: window.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testRunningDaemonShowsStoreAppsAndModels() {
        let window = launch("running")

        XCTAssertTrue(window.staticTexts["Running, version 0.1.0"].waitForExistence(timeout: 5))
        XCTAssertEqual(label("tile-store", in: window), "Store, 290.04 GB, 61 files")
        XCTAssertEqual(label("tile-shared", in: window), "Shared with apps, 290.07 GB, not stored twice")
        XCTAssertEqual(label("tile-archive", in: window), "Archive, Attached, /Volumes/Trunk/dedupd")
        XCTAssertTrue(anything(in: window, labelled: "Models (4)").exists, "the heading counts the models")
        XCTAssertTrue(
            anything(in: window, labelContains: "lmstudio-community/Qwen3.8-27B-MLX-4bit").exists,
            "a model is listed by name"
        )
        XCTAssertTrue(
            anything(in: window, labelContains: "8 stored · 0 in apps").exists,
            "a model no app holds is listed too"
        )
        XCTAssertFalse(window.buttons["pull-button"].isEnabled, "nothing to pull until a model is named")
        XCTAssertTrue(window.buttons["scan-button"].isEnabled)

        screenshot(window, "01-running")
    }

    func testPullAndScanShowTheirOutcome() {
        let window = launch("running")

        let field = window.textFields["pull-model"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.click()
        field.typeText("bartowski/SmolLM2-135M-Instruct-GGUF:Q4_K_M")
        let pull = window.buttons["pull-button"]
        XCTAssertTrue(pull.isEnabled)
        pull.click()

        XCTAssertTrue(
            window.staticTexts["4 files, 106.07 MB. Downloaded 4; 0 already in the store."].waitForExistence(timeout: 5),
            "the outcome of the pull"
        )
        XCTAssertTrue(window.staticTexts["Bionic: 4 placed as clones, 0 already there"].exists)

        window.buttons["scan-button"].click()
        XCTAssertTrue(window.staticTexts["Scan finished"].waitForExistence(timeout: 5))

        screenshot(window, "02-pull-and-scan")
    }

    func testDownloadInFlightShowsProgress() {
        let window = launch("downloading")

        XCTAssertTrue(
            label("download-progress", in: window).contains("Downloading: 41.94 MB of 106.07 MB"),
            "the progress of the download"
        )

        screenshot(window, "03-downloading")
    }

    func testNoDaemonExplainsHowToStartOne() {
        let window = launch("notRunning")

        XCTAssertTrue(window.staticTexts["The daemon is not running"].waitForExistence(timeout: 5))
        XCTAssertTrue(window.staticTexts["No dedupd daemon answers at 127.0.0.1:8642."].exists)
        XCTAssertTrue(window.staticTexts["dedupd install"].exists, "the command that starts the daemon")
        XCTAssertFalse(window.buttons["scan-button"].isEnabled)

        screenshot(window, "04-not-running")
    }

    /// The real thing: the app, the Rust client and a running daemon. Skipped
    /// unless a daemon's address is given, because it downloads a model:
    ///
    ///     just app-live 127.0.0.1:8643
    ///
    /// Point it at a daemon with a scratch home, not at the real one.
    func testLiveDaemonPullsAModel() throws {
        let environment = ProcessInfo.processInfo.environment
        let address = (environment["DEDUPD_LIVE_ADDRESS"] ?? "").trimmingCharacters(in: .whitespaces)
        try XCTSkipIf(address.isEmpty, "set DEDUPD_LIVE_ADDRESS to run against a daemon")
        let model = environment["DEDUPD_LIVE_MODEL"] ?? "bartowski/SmolLM2-135M-Instruct-GGUF:Q4_K_M"

        let window = launch(arguments: ["-panelWindow", "-daemonAddress", address])
        let summary = window.staticTexts["daemon-summary"]
        XCTAssertTrue(summary.waitForExistence(timeout: 10))
        XCTAssertTrue(
            text(in: window, containing: "Running, version").waitForExistence(timeout: 10),
            "the daemon at \(address) answers; the panel says: \(String(describing: summary.value))"
        )
        screenshot(window, "10-live-before")

        let field = window.textFields["pull-model"]
        field.click()
        field.typeText(model)
        window.buttons["pull-button"].click()

        let outcome = window.descendants(matching: .any)["pull-result"].firstMatch
        XCTAssertTrue(outcome.waitForExistence(timeout: 300), "the pull of \(model) finishes")
        XCTAssertTrue(text(in: window, containing: "placed as clones").exists, "the model was placed into an app")
        let repository = model.split(separator: ":").first.map(String.init) ?? model
        XCTAssertTrue(
            anything(in: window, labelContains: repository).waitForExistence(timeout: 10),
            "the model is listed after the pull"
        )
        screenshot(window, "11-live-after-pull")
    }
}
