import Foundation

/// Something that is loaded from the daemon and may fail.
enum Loaded<Value: Equatable & Sendable>: Equatable, Sendable {
    case idle
    case loading
    case ready(Value)
    case failed(String)

    var value: Value? {
        if case let .ready(value) = self {
            return value
        }
        return nil
    }

    var isLoading: Bool { self == .loading }

    var failure: String? {
        if case let .failed(message) = self {
            return message
        }
        return nil
    }
}
