import Testing

@testable import Dedupd

@Suite("The pull form")
struct PullFormTests {
    @Test(
        "Names the daemon accepts",
        arguments: [
            "bartowski/SmolLM2-135M-Instruct-GGUF",
            "bartowski/SmolLM2-135M-Instruct-GGUF:Q4_K_M",
            "mlx-community/Qwen2.5-0.5B-Instruct-4bit@main",
            "org/repo:Q8_0@refs/pr/1",
            "  org/repo  ",
        ]
    )
    func accepts(model: String) {
        var form = PullForm()
        form.model = model
        #expect(form.isValid)
        #expect(form.problem == nil)
        #expect(form.request != nil)
    }

    @Test(
        "Names it would refuse",
        arguments: ["repo-without-org", "org/", "/repo", "org/repo:", "org/re po", "https://huggingface.co/org/repo", "org/repo/extra"]
    )
    func refuses(model: String) {
        var form = PullForm()
        form.model = model
        #expect(!form.isValid)
        #expect(form.request == nil)
        #expect(form.problem != nil)
    }

    @Test("An empty form has no request and no complaint")
    func empty() {
        let form = PullForm()
        #expect(form.request == nil)
        #expect(form.problem == nil)
    }

    @Test("The request carries the trimmed name, the apps in a fixed order and the offline choice")
    func request() {
        var form = PullForm()
        form.model = " org/repo:Q4_K_M "
        form.apps = [.huggingFace, .ollama, .bionic]
        form.offline = true
        #expect(
            form.request == PullRequest(model: "org/repo:Q4_K_M", apps: ["bionic", "ollama", "hf"], include: [], offline: true)
        )
    }

    @Test("A pull into no app is allowed: the model goes to the store only")
    func noApps() {
        var form = PullForm()
        form.model = "org/repo"
        form.apps = []
        #expect(form.request == PullRequest(model: "org/repo", apps: []))
    }
}
