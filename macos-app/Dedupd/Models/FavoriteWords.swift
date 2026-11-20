import DedupdClient
import Foundation

extension Favorite: @retroactive Identifiable {
    public var id: String { repo }
}

extension LikesSync {
    /// Where the likes stand, in a sentence.
    var summary: String {
        if !hasToken {
            return "To bring in your likes from Hugging Face, sign in with hf auth login in Terminal."
        }
        if let problem {
            return problem
        }
        guard let user, let syncedAt else {
            return "Your likes on Hugging Face have not been read yet."
        }
        let when = TimeFormat.relative(TimeFormat.date(epoch: syncedAt))
        return "Likes of \(user) on Hugging Face, read \(when)."
    }
}

extension Favorite {
    var name: String { RepoName.name(repo) }
    var organisation: String { RepoName.organisation(repo) }

    /// Where it came from, in words.
    var from: String {
        switch source {
        case .dedupd: "Starred"
        case .huggingFace: "Liked on Hugging Face"
        }
    }
}
