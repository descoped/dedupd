import Foundation
import Testing

@testable import Dedupd

/// A model's name or address is read out of text a person pasted. The text
/// is not trusted: only a checked `org/name` ever comes out of it.
struct ModelAddressTests {
    @Test(
        "A repository's name or its address on Hugging Face names the repository",
        arguments: [
            ("org/repo", "org/repo"),
            ("org_x/Repo-1.5_b", "org_x/Repo-1.5_b"),
            ("  https://huggingface.co/org/repo  ", "org/repo"),
            ("https://huggingface.co/org/repo/", "org/repo"),
            ("https://huggingface.co/org/repo/tree/main", "org/repo"),
            ("https://huggingface.co/org/repo/blob/main/model.gguf#L1", "org/repo"),
            ("https://www.huggingface.co/org/repo?download=true", "org/repo"),
            ("HTTPS://HuggingFace.co/org/repo", "org/repo"),
            ("http://huggingface.co/org/repo", "org/repo"),
            ("https://hf.co/org/repo", "org/repo"),
            ("hf.co/org/repo", "org/repo"),
            ("huggingface.co/org/repo", "org/repo"),
        ]
    )
    func repositoryNames(text: String, repo: String) {
        #expect(ModelSpec.address(in: text) == .repository(repo))
        #expect(ModelSpec.repository(in: text) == repo)
    }

    @Test("Words are not a repository", arguments: ["qwen", "qwen gguf", "org/repo:Q4_K_M", "a/b c", "", "org", "org/repo/extra"])
    func words(text: String) {
        #expect(ModelSpec.address(in: text) == nil)
    }

    @Test(
        "An address that is made to mislead names nothing",
        arguments: [
            // Another host dressed up as Hugging Face.
            "https://huggingface.co@evil.example/org/repo",
            "https://user:secret@huggingface.co/org/repo",
            "https://huggingface.co.evil.example/org/repo",
            "https://evil.example/huggingface.co/org/repo",
            "https://evil.example/?next=https://huggingface.co/org/repo",
            "https://huggingface.co:8443/org/repo",
            "https://huggingface.co\\@evil.example/org/repo",
            "https://huggingf\u{0430}ce.co/org/repo",
            "https://hugging%66ace.co/org/repo",
            // Another scheme.
            "javascript://huggingface.co/org/repo",
            "file:///etc/passwd",
            "ftp://huggingface.co/org/repo",
            "dedupd://huggingface.co/org/repo",
            // A path that climbs out of the repository.
            "https://huggingface.co/org/..",
            "https://huggingface.co/../../etc/passwd",
            "https://huggingface.co/org/%2e%2e",
            "https://huggingface.co/org/repo%2F..%2F..",
            "https://huggingface.co//org/repo",
            "../etc/passwd",
            "org/..",
            "./repo",
            // Names Hugging Face does not allow.
            "org/repo.git",
            "org/re--po",
            "org/re..po",
            "org/-repo",
            "org/repo.",
            // Things that are not model repositories.
            "https://huggingface.co/datasets/org/data",
            "https://huggingface.co/spaces/org/app",
            "https://huggingface.co/api/models",
            "https://huggingface.co/org",
            "https://huggingface.co/",
            "huggingface.co/org",
            "hf.co/repo",
            // Text meant for a shell, a page or a log.
            "org/repo; rm -rf ~",
            "org/$(whoami)",
            "org/`id`",
            "org/<script>alert(1)</script>",
            "org/repo\nhttps://evil.example",
            "org/repo\u{0000}",
            "org/re\u{202E}po",
        ]
    )
    func misleading(text: String) {
        #expect(ModelSpec.address(in: text) == nil)
        #expect(ModelSpec.repository(in: text) == nil)
    }

    @Test("A name is at most 96 characters, and a text is not read at all past 512")
    func lengths() {
        let name = String(repeating: "a", count: 96)
        #expect(ModelSpec.repository(in: "org/\(name)") == "org/\(name)")
        #expect(ModelSpec.repository(in: "org/\(name)a") == nil)
        #expect(ModelSpec.address(in: "https://huggingface.co/org/repo?" + String(repeating: "x", count: 600)) == nil)
    }

    @Test(
        "An address at a source that is not built yet is named as such",
        arguments: [
            "https://modelscope.cn/models/Qwen/Qwen3.8-27B",
            "https://www.modelscope.cn/models/org/repo",
            "modelscope.ai/models/org/repo",
            "modelscope.cn/models",
        ]
    )
    func laterSources(text: String) {
        #expect(ModelSpec.address(in: text) == .unsupported("ModelScope"))
        #expect(ModelSpec.repository(in: text) == nil)
    }

    @Test("A search field's text is a model, words, or nothing to use")
    func typed() {
        #expect(ModelSpec.typed("") == .words(""))
        #expect(ModelSpec.typed("  qwen 3.5 gguf ") == .words("qwen 3.5 gguf"))
        #expect(ModelSpec.typed("org/repo") == .model("org/repo"))
        #expect(ModelSpec.typed("org/repo:Q4_K_M") == .model("org/repo"))
        #expect(ModelSpec.typed("org/repo@refs/pr/1") == .model("org/repo"))
        #expect(ModelSpec.typed(" https://hf.co/org/repo?download=true ") == .model("org/repo"))
    }

    @Test(
        "Text that looks like an address and is not a model's is not searched for",
        arguments: [
            "https://evil.example/org/repo",
            "https://huggingface.co@evil.example/org/repo",
            "https://modelscope.cn/models/org/repo",
            "file:///etc/passwd",
            "../etc/passwd",
            "org/a..b:Q4_K_M",
            "org/repo:Q4; rm -rf ~",
            "a/b c",
            "c:\\windows\\system32",
            "qwen\u{0007}",
            "qwen\u{202E}fugg",
            "line one\nline two",
        ]
    )
    func refused(text: String) {
        guard case .refused = ModelSpec.typed(text) else {
            Issue.record("\(text.debugDescription) was not refused: \(ModelSpec.typed(text))")
            return
        }
    }

    @Test("Words are searched for up to 200 characters")
    func longWords() {
        let words = String(repeating: "a", count: 200)
        #expect(ModelSpec.typed(words) == .words(words))
        guard case .refused = ModelSpec.typed(words + "a") else {
            Issue.record("201 characters were not refused")
            return
        }
    }
}
