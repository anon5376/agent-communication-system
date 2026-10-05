import Foundation

/// An untyped JSON value, used for request payloads sent to the acs-desktop
/// helper and for inspecting reply envelopes before typed decoding.
public enum JSONValue: Codable, Sendable, Equatable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case array([JSONValue])
    case object([String: JSONValue])
    case null

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else {
            throw DecodingError.dataCorruptedError(
                in: container, debugDescription: "Unsupported JSON value")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }
}

extension JSONValue {
    /// Convenience: true only for `.null`.
    var isNull: Bool {
        if case .null = self { return true }
        return false
    }

    /// Convenience: treat the value as a bool, or nil when it is not one.
    var bool: Bool? {
        if case .bool(let value) = self { return value }
        return nil
    }

    /// Convenience: treat the value as an object, or nil when it is not one.
    var objectValue: [String: JSONValue]? {
        if case .object(let value) = self { return value }
        return nil
    }
}
