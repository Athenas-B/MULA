'use strict';

const serverUrlInput = document.getElementById('serverUrl');
const saveBtn = document.getElementById('save');
const msgDiv = document.getElementById('msg');

chrome.storage.local.get({ serverUrl: 'http://127.0.0.1:8765' }, ({ serverUrl }) => {
  serverUrlInput.value = serverUrl;
});

saveBtn.onclick = () => {
  chrome.storage.local.set({ serverUrl: serverUrlInput.value.trim() }, () => {
    msgDiv.textContent = 'Saved.';
    setTimeout(() => { msgDiv.textContent = ''; }, 2000);
  });
};
