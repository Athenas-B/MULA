'use strict';

const serverUrlInput = document.getElementById('serverUrl');
const saveBtn = document.getElementById('save');
const msgDiv = document.getElementById('msg');

serverUrlInput.placeholder = VSD_DEFAULT_SERVER;

vsdGetServerUrl().then((serverUrl) => {
  serverUrlInput.value = serverUrl;
});

saveBtn.onclick = () => {
  chrome.storage.local.set({ serverUrl: serverUrlInput.value.trim() }, () => {
    msgDiv.textContent = 'Saved.';
    setTimeout(() => { msgDiv.textContent = ''; }, 2000);
  });
};
