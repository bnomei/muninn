import AVFAudio
import Foundation
import Speech

private struct HelperRequest: Decodable {
    let wavPath: String
    let locale: String?
    let installAssets: Bool
}

private struct HelperResponse: Encodable {
    let outcome: String
    let code: String
    let message: String
    let transcript: String?
    let resolvedLocale: String?
    let assetStatus: String?
}

private enum AssetReadiness {
    case ready(assetStatus: String)
    case unavailable(HelperResponse)
}

@main
struct AppleSpeechTranscriberHelper {
    static func main() async {
        emit(await run())
    }

    private static func run() async -> HelperResponse {
        let input = FileHandle.standardInput.readDataToEndOfFile()
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase

        let request: HelperRequest
        do {
            request = try decoder.decode(HelperRequest.self, from: input)
        } catch {
            return requestFailed(
                code: "invalid_apple_speech_helper_input",
                message: "failed to decode Apple Speech helper input JSON: \(error.localizedDescription)"
            )
        }

        return await process(request)
    }

    private static func process(_ request: HelperRequest) async -> HelperResponse {
        let osVersion = ProcessInfo.processInfo.operatingSystemVersion
        guard osVersion.majorVersion >= 26 else {
            return HelperResponse(
                outcome: "unavailable_platform",
                code: "unsupported_apple_speech_platform",
                message: "Apple Speech transcription requires macOS 26 or newer",
                transcript: nil,
                resolvedLocale: nil,
                assetStatus: nil
            )
        }

        guard SpeechTranscriber.isAvailable else {
            return HelperResponse(
                outcome: "unavailable_runtime_capability",
                code: "apple_speech_backend_unavailable",
                message: "Apple Speech transcription is unavailable on this system",
                transcript: nil,
                resolvedLocale: nil,
                assetStatus: nil
            )
        }

        let requestedLocaleIdentifier = request.locale?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let requestedLocale = requestedLocaleIdentifier.flatMap { identifier in
            identifier.isEmpty ? nil : Locale(identifier: identifier)
        } ?? Locale.current
        let requestedIdentifier = requestedLocale.identifier

        guard let resolvedLocale = await SpeechTranscriber.supportedLocale(
            equivalentTo: requestedLocale
        ) else {
            return HelperResponse(
                outcome: "unavailable_runtime_capability",
                code: "unsupported_apple_speech_locale",
                message: "Apple Speech does not support locale `\(requestedIdentifier)` on this system",
                transcript: nil,
                resolvedLocale: requestedIdentifier,
                assetStatus: nil
            )
        }

        let transcriber = SpeechTranscriber(locale: resolvedLocale, preset: .transcription)
        let modules: [any SpeechModule] = [transcriber]
        let readiness = await ensureAssetsReady(
            for: modules,
            locale: resolvedLocale,
            installAssets: request.installAssets
        )
        switch readiness {
        case .unavailable(let response):
            return response
        case .ready(let assetStatus):
            let url = URL(fileURLWithPath: request.wavPath)

            let audioFile: AVAudioFile
            do {
                audioFile = try AVAudioFile(forReading: url)
            } catch {
                return requestFailed(
                    code: "apple_speech_audio_file_open_failed",
                    message: "failed to open audio file at \(request.wavPath): \(error.localizedDescription)",
                    resolvedLocale: resolvedLocale.identifier,
                    assetStatus: assetStatus
                )
            }

            let analyzer = SpeechAnalyzer(modules: modules)
            let transcriptTask = Task {
                try await collectBestTranscript(from: transcriber)
            }

            do {
                try await analyzer.start(inputAudioFile: audioFile, finishAfterFile: true)
                let transcript = try await transcriptTask.value
                let trimmed = transcript.trimmingCharacters(in: .whitespacesAndNewlines)

                if trimmed.isEmpty {
                    return HelperResponse(
                        outcome: "empty_transcript",
                        code: "empty_transcript_text",
                        message: "Apple Speech transcription returned an empty transcript",
                        transcript: nil,
                        resolvedLocale: resolvedLocale.identifier,
                        assetStatus: assetStatus
                    )
                }

                return HelperResponse(
                    outcome: "produced_transcript",
                    code: "produced_transcript",
                    message: "Apple Speech transcription produced transcript text",
                    transcript: trimmed,
                    resolvedLocale: resolvedLocale.identifier,
                    assetStatus: assetStatus
                )
            } catch {
                transcriptTask.cancel()
                return requestFailed(
                    code: "apple_speech_transcription_failed",
                    message: "Apple Speech transcription failed: \(error.localizedDescription)",
                    resolvedLocale: resolvedLocale.identifier,
                    assetStatus: assetStatus
                )
            }
        }
    }

    private static func ensureAssetsReady(
        for modules: [any SpeechModule],
        locale: Locale,
        installAssets: Bool
    ) async -> AssetReadiness {
        let initialStatus = await AssetInventory.status(forModules: modules)
        switch initialStatus {
        case .installed:
            return .ready(assetStatus: assetStatusString(initialStatus))
        case .unsupported:
            return .unavailable(
                HelperResponse(
                    outcome: "unavailable_assets",
                    code: "unsupported_apple_speech_assets",
                    message: "Apple-managed Speech assets are unavailable for locale `\(locale.identifier)` on this system",
                    transcript: nil,
                    resolvedLocale: locale.identifier,
                    assetStatus: assetStatusString(initialStatus)
                )
            )
        case .supported, .downloading:
            guard installAssets else {
                return .unavailable(
                    HelperResponse(
                        outcome: "unavailable_assets",
                        code: "apple_speech_assets_not_installed",
                        message: "Apple-managed Speech assets for locale `\(locale.identifier)` are not installed; enable providers.apple_speech.install_assets or install the required assets in macOS",
                        transcript: nil,
                        resolvedLocale: locale.identifier,
                        assetStatus: assetStatusString(initialStatus)
                    )
                )
            }

            do {
                if let request = try await AssetInventory.assetInstallationRequest(
                    supporting: modules
                ) {
                    try await request.downloadAndInstall()
                }
            } catch {
                return .unavailable(
                    HelperResponse(
                        outcome: "unavailable_assets",
                        code: "apple_speech_assets_install_failed",
                        message: "failed to install Apple-managed Speech assets for locale `\(locale.identifier)`: \(error.localizedDescription)",
                        transcript: nil,
                        resolvedLocale: locale.identifier,
                        assetStatus: assetStatusString(initialStatus)
                    )
                )
            }

            let finalStatus = await AssetInventory.status(forModules: modules)
            guard finalStatus == .installed else {
                return .unavailable(
                    HelperResponse(
                        outcome: "unavailable_assets",
                        code: "apple_speech_assets_not_installed",
                        message: "Apple-managed Speech assets for locale `\(locale.identifier)` are still unavailable after installation",
                        transcript: nil,
                        resolvedLocale: locale.identifier,
                        assetStatus: assetStatusString(finalStatus)
                    )
                )
            }

            return .ready(assetStatus: assetStatusString(finalStatus))
        }
    }

    private static func collectBestTranscript(from transcriber: SpeechTranscriber) async throws -> String {
        var bestTranscript = ""

        for try await result in transcriber.results {
            guard result.isFinal else {
                continue
            }

            let transcript = String(result.text.characters)
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if transcript.count >= bestTranscript.count {
                bestTranscript = transcript
            }
        }

        return bestTranscript
    }

    private static func requestFailed(
        code: String,
        message: String,
        resolvedLocale: String? = nil,
        assetStatus: String? = nil
    ) -> HelperResponse {
        HelperResponse(
            outcome: "request_failed",
            code: code,
            message: message,
            transcript: nil,
            resolvedLocale: resolvedLocale,
            assetStatus: assetStatus
        )
    }

    private static func assetStatusString(_ status: AssetInventory.Status) -> String {
        switch status {
        case .unsupported:
            return "unsupported"
        case .supported:
            return "supported"
        case .downloading:
            return "downloading"
        case .installed:
            return "installed"
        }
    }

    private static func emit(_ response: HelperResponse) {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        encoder.keyEncodingStrategy = .convertToSnakeCase

        guard let data = try? encoder.encode(response) else {
            let fallback = "{\"outcome\":\"request_failed\",\"code\":\"apple_speech_response_encode_failed\",\"message\":\"failed to encode Apple Speech helper response\",\"transcript\":null,\"resolved_locale\":null,\"asset_status\":null}\n"
            FileHandle.standardOutput.write(Data(fallback.utf8))
            return
        }

        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
}
