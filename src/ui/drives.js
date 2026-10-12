// Drive Test tab: drive inventory, SMART data and self-tests, and full format.

import { invoke } from "./tauri.js";
import { errorText, escapeHtml, setCollapsed } from "./dom.js";

export const state = {
  drives: [],
  smartLoadedId: null,
  testPollInterval: null,
  testPollNoProgressCount: 0,
  formatTimer: null,
};

export async function initDriveTest() {
  const select = document.getElementById("drive-select");
  const refreshBtn = document.getElementById("drive-refresh");
  const toggle = document.getElementById("drive-toggle");
  const content = document.getElementById("drive-info-content");
  const smartToggle = document.getElementById("smart-toggle");
  const smartContent = document.getElementById("smart-content");
  const smartRetry = document.getElementById("smart-retry-admin");
  const smartRunTest = document.getElementById("smart-run-test");
  const smartCheckStatus = document.getElementById("smart-check-status");
  const formatBtn = document.getElementById("drive-format-btn");
  if (!select || !refreshBtn) return;

  select.addEventListener("change", onDriveSelect);
  refreshBtn.addEventListener("click", loadDrives);
  if (toggle && content) {
    toggle.addEventListener("click", () => setCollapsed(toggle, content));
  }
  if (smartToggle && smartContent) {
    smartToggle.addEventListener("click", () => onSmartToggle(smartToggle, smartContent));
  }
  if (smartRetry) {
    smartRetry.addEventListener("click", () => onSmartRetryAdmin());
  }
  if (smartRunTest) {
    smartRunTest.addEventListener("click", () => onRunDriveTest());
  }
  if (smartCheckStatus) {
    smartCheckStatus.addEventListener("click", () => onCheckDriveTestStatus());
  }
  if (formatBtn) {
    formatBtn.addEventListener("click", () => onFormatDrive());
  }

  // Enumerating drives runs PowerShell for a few seconds; wait until the tab is opened.
  const driveTab = document.querySelector('.tab[data-tab="drivetest"]');
  let drivesLoaded = false;
  driveTab?.addEventListener("click", () => {
    if (drivesLoaded) return;
    drivesLoaded = true;
    loadDrives();
  });
}

async function loadDrives() {
  const select = document.getElementById("drive-select");
  const empty = document.getElementById("drive-empty");
  const details = document.getElementById("drive-details");
  const tbody = document.querySelector("#drive-info-table tbody");

  select.disabled = true;
  select.innerHTML = '<option value="">Loading drives...</option>';

  try {
    const drives = await invoke("list_physical_drives");
    state.drives = drives || [];
    select.innerHTML = '';

    if (state.drives.length === 0) {
      select.innerHTML = '<option value="">No physical drives found</option>';
      empty.textContent = "No physical drives found.";
      empty.classList.remove("hidden");
      details.classList.add("hidden");
      return;
    }

    const defaultOpt = document.createElement("option");
    defaultOpt.value = "";
    defaultOpt.textContent = "Select a drive...";
    select.appendChild(defaultOpt);

    for (const d of state.drives) {
      const opt = document.createElement("option");
      opt.value = d.device_id;
      const letters = d.drive_letters?.length ? ` [${d.drive_letters.join(" ")}]` : "";
      opt.textContent = `${d.model} (${d.size_text})${letters}`;
      select.appendChild(opt);
    }

    empty.classList.remove("hidden");
    empty.textContent = "Select a drive to see details.";
    details.classList.add("hidden");
    tbody.innerHTML = "";
  } catch (err) {
    console.error("Failed to list drives:", err);
    select.innerHTML = '<option value="">Failed to load drives</option>';
    empty.textContent = `Error: ${errorText(err)}`;
    empty.classList.remove("hidden");
    details.classList.add("hidden");
  } finally {
    select.disabled = false;
  }
}

function stopFormatTimer() {
  if (state.formatTimer) {
    clearInterval(state.formatTimer);
    state.formatTimer = null;
  }
}

function startFormatTimer(statusEl) {
  stopFormatTimer();
  const start = Date.now();
  state.formatTimer = setInterval(() => {
    const elapsed = Math.floor((Date.now() - start) / 1000);
    const m = Math.floor(elapsed / 60).toString().padStart(2, "0");
    const s = (elapsed % 60).toString().padStart(2, "0");
    if (statusEl) {
      statusEl.textContent = `Formatting... (elapsed ${m}:${s}). Do not close the app.`;
      statusEl.className = "drive-format-status";
    }
  }, 1000);
}

function stopTestProgressPolling() {
  if (state.testPollInterval) {
    clearInterval(state.testPollInterval);
    state.testPollInterval = null;
  }
  state.testPollNoProgressCount = 0;
}

// smartmontools is never installed silently: if smartctl is missing, ask the user first.
export async function invokeSmart(command, args) {
  try {
    return await invoke(command, args);
  } catch (err) {
    if (err?.kind !== "smartctl_missing") throw err;
    const install = window.confirm(
      "SMART features need smartmontools (smartctl), which is not installed.\n\n" +
        "Install it now with winget? This accepts the smartmontools package license."
    );
    if (!install) {
      throw new Error("smartmontools is not installed. Install it from https://www.smartmontools.org/ to use SMART features.");
    }
    await invoke("install_smartmontools");
    return await invoke(command, args);
  }
}

export async function loadSmartData(id, elevated) {
  const smartData = document.getElementById("smart-data");
  const smartStatus = document.getElementById("smart-status");
  const smartAdmin = document.getElementById("smart-admin");

  if (smartAdmin) smartAdmin.classList.add("hidden");
  smartData.textContent = "";
  smartStatus.textContent = elevated ? "Loading SMART data with administrator privileges..." : "Loading SMART data...";
  smartStatus.classList.remove("hidden");
  try {
    smartData.textContent = await invokeSmart("get_drive_smart", { id, elevated });
    state.smartLoadedId = id;
  } catch (err) {
    console.error(err);
    smartData.textContent = `Error: ${errorText(err)}`;
    if (smartAdmin && err?.kind === "needs_elevation") smartAdmin.classList.remove("hidden");
  } finally {
    smartStatus.classList.add("hidden");
  }
}

export async function onSmartRetryAdmin() {
  const id = document.getElementById("drive-select").value;
  if (id) await loadSmartData(id, true);
}

async function onSmartToggle(toggle, content) {
  const id = document.getElementById("drive-select").value;
  const smartAdmin = document.getElementById("smart-admin");

  const collapsed = setCollapsed(toggle, content);
  if (smartAdmin) smartAdmin.classList.add("hidden");

  if (!collapsed && id && state.smartLoadedId !== id) await loadSmartData(id, false);
}

export async function onRunDriveTest() {
  const select = document.getElementById("drive-select");
  const testCheckboxes = document.querySelectorAll('input[name="smart-test-type"]:checked');
  const saveBefore = document.querySelector('input[name="smart-test-save"][value="before"]')?.checked;
  const saveAfter = document.querySelector('input[name="smart-test-save"][value="after"]')?.checked;
  const testStatus = document.getElementById("smart-test-status");
  const runBtn = document.getElementById("smart-run-test");
  const id = select.value;
  const types = Array.from(testCheckboxes).map((cb) => cb.value);

  if (!id || types.length === 0) {
    if (testStatus) {
      testStatus.textContent = "Select at least one test type to run";
      testStatus.className = "smart-test-status error";
    }
    return;
  }

  stopTestProgressPolling();

  if (testStatus) {
    testStatus.textContent = "Starting tests with administrator privileges...";
    testStatus.className = "smart-test-status";
  }
  if (runBtn) runBtn.disabled = true;

  const lines = [];
  let hasError = false;
  const appendLine = (line, isError) => {
    lines.push(line);
    if (isError) hasError = true;
    if (testStatus) testStatus.textContent = lines.join("\n");
  };

  try {
    if (saveBefore) {
      const path = await invokeSmart("save_smart_snapshot", { id, label: "before" });
      appendLine(`SMART snapshot saved before tests:\n  ${path}`);
    }

    for (const type of types) {
      try {
        const output = await invokeSmart("run_drive_test", { id, testType: type });
        appendLine(`${type}: started\n  ${output}`);
      } catch (err) {
        appendLine(`${type}: error - ${errorText(err)}`, true);
      }
    }

    if (saveAfter) {
      const path = await invokeSmart("save_smart_snapshot", { id, label: "after" });
      appendLine(`SMART snapshot saved after tests:\n  ${path}`);
    }

    if (testStatus) {
      testStatus.className = hasError ? "smart-test-status error" : "smart-test-status success";
    }
    if (!hasError) {
      setTimeout(() => startTestProgressPolling(id), 2000);
    }
  } catch (err) {
    console.error(err);
    if (testStatus) {
      testStatus.textContent = `Error: ${errorText(err)}`;
      testStatus.className = "smart-test-status error";
    }
  } finally {
    if (runBtn) runBtn.disabled = false;
  }
}

/** Text for a self-test status from `get_drive_self_test_status`. */
export function formatSelfTestStatus(status) {
  const log = status.log.length
    ? status.log
        .map((e) => `  ${e.test}: ${e.result}${e.hours != null ? ` (at ${e.hours} h)` : ""}`)
        .join("\n")
    : "  No self-tests logged";
  return `Self-test status: ${status.status}\n\nSelf-test log (most recent first):\n${log}`;
}

function startTestProgressPolling(id) {
  stopTestProgressPolling();
  const testStatus = document.getElementById("smart-test-status");
  if (testStatus) {
    testStatus.textContent = "Monitoring self-test progress...";
    testStatus.className = "smart-test-status";
  }
  pollTestStatus(id);
  state.testPollInterval = setInterval(() => pollTestStatus(id), 10000);
}

export async function pollTestStatus(id) {
  const select = document.getElementById("drive-select");
  const testStatus = document.getElementById("smart-test-status");
  if (!id || select.value !== id) {
    stopTestProgressPolling();
    return;
  }

  try {
    const status = await invokeSmart("get_drive_self_test_status", { id });

    let header;
    if (status.in_progress) {
      header =
        status.percent_remaining != null
          ? `Monitoring self-test progress... (${status.percent_remaining}% remaining, updates every 10s)`
          : "Monitoring self-test progress... (updates every 10s)";
    } else if (status.failed) {
      header = "Self-test failed";
    } else {
      header = "Self-test finished";
    }

    if (testStatus) {
      testStatus.textContent = `${header}\n\n${formatSelfTestStatus(status)}`;
      testStatus.className = status.failed ? "smart-test-status error" : "smart-test-status";
    }

    if (status.failed) {
      stopTestProgressPolling();
      return;
    }

    if (status.in_progress) {
      state.testPollNoProgressCount = 0;
    } else {
      state.testPollNoProgressCount += 1;
      if (state.testPollNoProgressCount >= 2) {
        if (testStatus) testStatus.className = "smart-test-status success";
        stopTestProgressPolling();
      }
    }
  } catch (err) {
    console.error(err);
    if (testStatus) {
      testStatus.textContent = `Error checking status: ${errorText(err)}`;
      testStatus.className = "smart-test-status error";
    }
    stopTestProgressPolling();
  }
}

async function onCheckDriveTestStatus() {
  const select = document.getElementById("drive-select");
  const testStatus = document.getElementById("smart-test-status");
  const checkBtn = document.getElementById("smart-check-status");
  const id = select.value;
  if (!id) return;

  if (testStatus) {
    testStatus.textContent = "Checking test status with administrator privileges...";
    testStatus.className = "smart-test-status";
  }
  if (checkBtn) checkBtn.disabled = true;

  try {
    const status = await invokeSmart("get_drive_self_test_status", { id });
    if (testStatus) {
      testStatus.textContent = formatSelfTestStatus(status);
      testStatus.className = status.failed
        ? "smart-test-status error"
        : status.in_progress
          ? "smart-test-status"
          : "smart-test-status success";
    }
  } catch (err) {
    console.error(err);
    if (testStatus) {
      testStatus.textContent = `Error: ${errorText(err)}`;
      testStatus.className = "smart-test-status error";
    }
  } finally {
    if (checkBtn) checkBtn.disabled = false;
  }
}

export async function onFormatDrive() {
  const select = document.getElementById("drive-select");
  const formatStatus = document.getElementById("drive-format-status");
  const formatBtn = document.getElementById("drive-format-btn");
  const id = select.value;
  if (!id) return;

  stopTestProgressPolling();

  const drive = state.drives.find((d) => d.device_id === id);
  if (drive?.is_system) return;
  const driveLabel = drive ? `${drive.vendor || ""} ${drive.model || ""} ${drive.serial || ""}`.trim() || id : id;

  const confirmed = window.confirm(
    `WARNING: This will permanently erase ALL data on the selected drive.\n\n` +
      `Drive: ${driveLabel}\n\n` +
      `This action will remove all partitions, create a new single partition, and perform a full format.\n` +
      `Windows will ask for administrator rights.\n` +
      `It cannot be undone.\n\nDo you want to continue?`
  );
  if (!confirmed) return;

  if (formatBtn) formatBtn.disabled = true;
  startFormatTimer(formatStatus);

  try {
    const result = await invoke("format_drive", { id });
    stopFormatTimer();
    await loadDrives();
    if (select) select.value = id;
    onDriveSelect();
    if (formatStatus) {
      formatStatus.textContent = result;
      formatStatus.className = "drive-format-status success";
    }
  } catch (err) {
    stopFormatTimer();
    console.error(err);
    if (formatStatus) {
      formatStatus.textContent = `Error: ${errorText(err)}`;
      formatStatus.className = "drive-format-status error";
    }
  } finally {
    if (formatBtn) formatBtn.disabled = !!state.drives.find((d) => d.device_id === id)?.is_system;
  }
}

export function onDriveSelect() {
  stopTestProgressPolling();
  const select = document.getElementById("drive-select");
  const id = select.value;
  const empty = document.getElementById("drive-empty");
  const details = document.getElementById("drive-details");
  const tbody = document.querySelector("#drive-info-table tbody");
  const smartToggle = document.getElementById("smart-toggle");
  const smartContent = document.getElementById("smart-content");
  const smartData = document.getElementById("smart-data");
  const smartAdmin = document.getElementById("smart-admin");
  const smartTest = document.getElementById("smart-test");
  const runBtn = document.getElementById("smart-run-test");
  const checkBtn = document.getElementById("smart-check-status");
  const formatSection = document.getElementById("drive-format");
  const formatStatus = document.getElementById("drive-format-status");

  state.smartLoadedId = null;
  smartData.textContent = "Select a drive and expand to load SMART data.";
  if (smartToggle && smartContent) setCollapsed(smartToggle, smartContent, true);
  if (smartAdmin) smartAdmin.classList.add("hidden");

  if (!id) {
    if (smartTest) smartTest.classList.add("hidden");
    if (formatSection) {
      formatSection.classList.add("hidden");
      formatStatus.textContent = "";
    }
    empty.classList.remove("hidden");
    details.classList.add("hidden");
    tbody.innerHTML = "";
    return;
  }

  const drive = state.drives.find((d) => d.device_id === id);
  if (!drive) {
    empty.classList.remove("hidden");
    details.classList.add("hidden");
    return;
  }

  const fmtList = (arr) => (arr?.length ? arr.join(", ") : "None");

  const fields = [
    ["Device", drive.device_id],
    ["Vendor", drive.vendor],
    ["Model", drive.model],
    ["Serial", drive.serial || "N/A"],
    ["Bus", drive.bus_type],
    ["Connection speed", drive.connection_speed],
    ["Media type", drive.media_type],
    ["Interface", drive.interface_type],
    ["Size", `${drive.size_text} (${drive.size.toLocaleString()} bytes)`],
    ["Partitions", drive.partitions.toString()],
    ["Drive letters", fmtList(drive.drive_letters)],
    ["Mount points", fmtList(drive.mount_points)],
    ["Health", drive.health_status || "N/A"],
    ["SMART capable", drive.smart_capable || "Unknown"],
    ["TRIM capable", drive.trim_capable || "Unknown"],
    ["Status", drive.status],
    ["Firmware", drive.firmware || "N/A"],
    ["PNP ID", drive.pnp_device_id || "N/A"],
  ];

  tbody.innerHTML = fields
    .map(([label, value]) => `<tr><td class="label">${label}</td><td class="value">${escapeHtml(value || "N/A")}</td></tr>`)
    .join("");

  if (smartTest) {
    smartTest.classList.remove("hidden");
  }
  if (formatSection) {
    formatSection.classList.remove("hidden");
    const formatBtn = document.getElementById("drive-format-btn");
    if (formatBtn) formatBtn.disabled = !!drive.is_system;
    if (formatStatus) {
      formatStatus.textContent = drive.is_system
        ? "This disk holds the running Windows installation and cannot be formatted."
        : "";
      formatStatus.className = "drive-format-status";
    }
  }
  if (runBtn) runBtn.disabled = false;
  if (checkBtn) checkBtn.disabled = false;

  empty.classList.add("hidden");
  details.classList.remove("hidden");
}
