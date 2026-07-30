; iHub's local development installer verifies the exact NSS-patched executable
; that makensis embeds. Tauri restores target/release/ihub.exe to its unbundled
; marker after packaging, so that restored file cannot be the payload trust
; anchor. These hooks snapshot the makensis input, bind it to a random nonce,
; and leave a small post-install marker for an independent hash comparison.

!define IHUB_INSTALLER_HOOK_DIR "${__FILEDIR__}"
!define IHUB_INSTALL_PROOF_MARKER ".ihub-install-proof.json"

!macro NSIS_HOOK_PREINSTALL
  ; Tauri's following File instruction has no /oname option, so the immutable
  ; snapshot deliberately preserves the application's installed file name.
  !system `powershell.exe -NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "${IHUB_INSTALLER_HOOK_DIR}\write-nsis-payload-proof.ps1" -PayloadPath "${MAINBINARYSRCPATH}" -SnapshotPath "${MAINBINARYNAME}.exe" -ProofPath "${OUTFILE}.ihub-payload-proof.json" -IncludePath "${OUTFILE}.ihub-payload-proof.nsh"` = 0
  !include "${OUTFILE}.ihub-payload-proof.nsh"

  ; File in Tauri's template is processed immediately after this macro. Point
  ; it at the immutable snapshot whose hash and nonce were just recorded.
  !undef MAINBINARYSRCPATH
  !define MAINBINARYSRCPATH "${IHUB_NSIS_PAYLOAD_SNAPSHOT}"

  ; A same-version reinstall must not satisfy verification with an old marker.
  Delete "$INSTDIR\${IHUB_INSTALL_PROOF_MARKER}"
  ; Remove the exact stray filename produced by an early development build of
  ; this proof hook. No wildcard or directory cleanup is performed.
  Delete "$INSTDIR\nsis-output.exe.ihub-main-binary.exe"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ClearErrors
  FileOpen $0 "$INSTDIR\${IHUB_INSTALL_PROOF_MARKER}" w
  IfErrors ihub_payload_proof_write_failed
  FileWriteUTF16LE /BOM $0 "{$\"schemaVersion$\":1,$\"managedBy$\":$\"iHub NSIS payload proof v1$\",$\"payloadSha256$\":$\"${IHUB_NSIS_PAYLOAD_SHA256}$\",$\"payloadLength$\":${IHUB_NSIS_PAYLOAD_LENGTH},$\"nonce$\":$\"${IHUB_NSIS_PAYLOAD_NONCE}$\"}"
  FileClose $0
  IfErrors ihub_payload_proof_write_failed
  Goto ihub_payload_proof_write_done

  ihub_payload_proof_write_failed:
    Abort "Could not write the iHub installed-payload proof marker."
  ihub_payload_proof_write_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$INSTDIR\${IHUB_INSTALL_PROOF_MARKER}"
  Delete "$INSTDIR\nsis-output.exe.ihub-main-binary.exe"
!macroend
