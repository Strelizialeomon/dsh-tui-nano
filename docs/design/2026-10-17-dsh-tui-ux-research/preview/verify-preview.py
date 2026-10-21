"""Bounded browser check of the local, offline terminal specimen.
Uses an isolated Chrome profile; does not read browser sessions or run model/tools.
Run: python3 verify-preview.py
"""
import base64
import json
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request
import websocket

ROOT = Path(__file__).resolve().parent
CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'

class DevTools:
    def __init__(self, url):
        self.socket = websocket.create_connection(url, timeout=15, origin='http://localhost')
        self.seq = 0
        self.errors = []
        self.requests = []

    def call(self, method, params=None):
        self.seq += 1
        identifier = self.seq
        self.socket.send(json.dumps({'id': identifier, 'method': method, 'params': params or {}}))
        while True:
            event = json.loads(self.socket.recv())
            if event.get('method') == 'Runtime.exceptionThrown':
                self.errors.append(event['params'])
            if event.get('method') == 'Network.requestWillBeSent':
                self.requests.append(event['params']['request']['url'])
            if event.get('id') != identifier:
                continue
            if 'error' in event:
                raise RuntimeError(event['error'])
            return event.get('result', {})

    def js(self, expression):
        result = self.call('Runtime.evaluate', {
            'expression': expression, 'returnByValue': True, 'awaitPromise': True,
        })
        if 'exceptionDetails' in result:
            raise RuntimeError(result['exceptionDetails'])
        return result.get('result', {}).get('value')

    def settle(self):
        self.js('new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))')

    def click(self, selector):
        self.js(f'document.querySelector({json.dumps(selector)}).click()')
        self.settle()

    def mouse_click(self, selector):
        point = self.js(f'''(() => {{
            const element=document.querySelector({json.dumps(selector)});
            element.scrollIntoView({{block:'nearest',inline:'nearest'}});
            const rect=element.getBoundingClientRect();
            return {{x:rect.left+rect.width/2,y:rect.top+rect.height/2}};
        }})()''')
        self.call('Input.dispatchMouseEvent', {'type':'mousePressed','button':'left','clickCount':1,**point})
        self.call('Input.dispatchMouseEvent', {'type':'mouseReleased','button':'left','clickCount':1,**point})
        self.settle()

    def press(self, key, shift=False):
        codes={'Enter':13,'Escape':27,'Tab':9,'ArrowUp':38,'ArrowDown':40,'ArrowLeft':37,'ArrowRight':39,'PageDown':34,' ':32}
        payload={'key':key,'windowsVirtualKeyCode':codes.get(key,ord(key.upper()) if len(key)==1 else 0),'modifiers':8 if shift else 0}
        if len(key)==1: payload['text']=key
        if key=='Enter': payload['text']='\r'
        self.call('Input.dispatchKeyEvent',{'type':'keyDown',**payload})
        self.call('Input.dispatchKeyEvent',{'type':'keyUp',**payload})
        self.settle()

    def capture(self, name):
        data = self.call('Page.captureScreenshot', {'format': 'png', 'captureBeyondViewport': False})
        (ROOT / name).write_bytes(base64.b64decode(data['data']))

results = []
def check(label, condition):
    results.append({'check': label, 'passed': bool(condition)})

with tempfile.TemporaryDirectory(prefix='dsh-ui-browser-') as browser_dir:
    with open(Path(browser_dir) / 'chrome.log', 'w') as browser_log:
        process = subprocess.Popen([
            CHROME, '--headless=new', '--disable-gpu', '--no-first-run',
            '--no-default-browser-check', '--disable-background-networking',
            '--remote-debugging-port=0', '--remote-allow-origins=*',
            '--user-data-dir=' + browser_dir, 'about:blank',
        ], stdout=browser_log, stderr=browser_log)
        devtools = None
        try:
            port_file = Path(browser_dir) / 'DevToolsActivePort'
            deadline = time.monotonic() + 15
            while not port_file.exists():
                if time.monotonic() > deadline or process.poll() is not None:
                    raise RuntimeError('Chrome did not expose its local debug port')
                time.sleep(.05)
            port = port_file.read_text().splitlines()[0]
            with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/list') as response:
                pages = json.load(response)
            devtools = DevTools(next(page['webSocketDebuggerUrl'] for page in pages if page['type'] == 'page'))
            devtools.call('Page.enable')
            devtools.call('Runtime.enable')
            devtools.call('Network.enable')
            devtools.call('Emulation.setDeviceMetricsOverride', {'width': 1440, 'height': 1060, 'deviceScaleFactor': 1, 'mobile': False})
            devtools.call('Emulation.setEmulatedMedia', {'features': [{'name':'prefers-reduced-motion','value':'reduce'}]})
            devtools.call('Page.navigate', {'url': (ROOT / 'index.html').as_uri()})
            deadline = time.monotonic() + 10
            while not devtools.js('Boolean(window.previewSnapshot)'):
                if time.monotonic() > deadline:
                    raise RuntimeError('Preview did not become ready')
                time.sleep(.05)
            devtools.js('document.fonts.ready')
            devtools.settle()

            check('default details collapsed', devtools.js('document.querySelectorAll("details[open]").length === 0'))
            check('terminal has no pictographic emoji', devtools.js('!(/\\p{Extended_Pictographic}/u.test(document.querySelector("#terminal").textContent))'))
            check('footer is reduced to permission and working directory', devtools.js('!document.querySelector(".status").textContent.includes("DeepSeek") && !document.querySelector(".status").textContent.includes("上下文") && !document.querySelector(".status").textContent.includes("Ctrl+O") && document.querySelector(".status").textContent.includes("权限") && document.querySelector(".status").textContent.includes("~/code/dsh-TUI")'))
            check('context occupancy is visible as a partial pair of input borders', devtools.js('(() => { const composer=document.querySelector(".composer"), top=getComputedStyle(composer,"::before"), bottom=getComputedStyle(composer,"::after"); return parseFloat(top.width)>0 && parseFloat(top.width)<composer.getBoundingClientRect().width && top.borderTopColor===bottom.borderTopColor; })()'))

            devtools.click('[data-detail="bash"] summary')
            check('single tool opens without opening others', devtools.js('document.querySelectorAll("details[open]").length === 1 && document.querySelector("[data-detail=bash]").open'))
            # Replace clipboard writes inside the specimen so verification never changes the OS clipboard.
            devtools.js('Object.defineProperty(navigator,"clipboard",{value:{writeText:async text=>{window.copiedSpecimenText=text}},configurable:true})')
            devtools.click('[data-copy]')
            check('copy returns the entire 11-line script', devtools.js('window.copiedSpecimenText.split("\\n").length === 11 && window.copiedSpecimenText.endsWith("\\nJS")'))
            devtools.capture('expanded-100-dark.png')
            devtools.click('[data-detail="bash"] .detail pre')
            check('clicking Bash script body collapses the whole card', devtools.js('!document.querySelector("[data-detail=bash]").open'))
            devtools.click('[data-key="result-summary"]')
            check('clicking Bash output preview expands the whole card', devtools.js('document.querySelector("[data-detail=bash]").open'))
            devtools.js('document.querySelector(\'[data-detail="bash"] .detail pre\').scrollIntoView({block:"center"})')
            devtools.settle()
            drag = devtools.js('''(() => { const r=document.querySelector('[data-detail="bash"] .detail pre').getBoundingClientRect(); return {x:r.left+8,y:r.top+33}; })()''')
            devtools.call('Input.dispatchMouseEvent', {'type':'mouseMoved', **drag})
            devtools.call('Input.dispatchMouseEvent', {'type':'mousePressed', 'button':'left', 'buttons':1, 'clickCount':1, **drag})
            for step in range(1,6):
                devtools.call('Input.dispatchMouseEvent', {'type':'mouseMoved', 'x':drag['x']+30*step, 'y':drag['y'], 'button':'left', 'buttons':1})
            devtools.settle()
            devtools.call('Input.dispatchMouseEvent', {'type':'mouseReleased', 'x':drag['x']+150, 'y':drag['y'], 'button':'left', 'buttons':0, 'clickCount':1})
            devtools.settle()
            drag_result=devtools.js('({open:document.querySelector("[data-detail=bash]").open,selected:String(window.getSelection())})')
            check('drag-selecting Bash code does not collapse the card', drag_result['open'] and len(drag_result['selected'])>0)
            if not results[-1]['passed']:
                results[-1]['details']={'gesture':drag,'result':drag_result}
            devtools.js('window.getSelection().removeAllRanges(); document.querySelector("#transcript").dispatchEvent(new PointerEvent("pointerdown",{bubbles:true}))')
            devtools.click('[data-scene="running"]')
            check('explicit tool expansion survives state change', devtools.js('document.querySelector("[data-detail=bash]").open'))
            devtools.click('#toggle-all')
            devtools.click('#toggle-all')
            check('global expansion can be closed', devtools.js('document.querySelectorAll("details[open]").length === 0'))
            check('running tail occupies exactly two visual rows', devtools.js('document.querySelector(".live-tail").getBoundingClientRect().height === 2 * parseFloat(getComputedStyle(document.querySelector(".terminal")).lineHeight)'))
            check('one running indicator', devtools.js('document.querySelectorAll(".activity-glyph").length === 1'))
            devtools.capture('running-100-dark.png')

            devtools.click('#history')
            devtools.js('document.querySelector("#transcript").scrollTop = 150')
            devtools.settle()
            before = devtools.js('previewSnapshot().scrollTop')
            devtools.click('#append')
            after = devtools.js('previewSnapshot().scrollTop')
            check('reading history stays anchored after new sample output', abs(before-after) < 1)
            check('jump-to-latest shown while reading history', devtools.js('!document.querySelector("#jump").hidden'))
            devtools.click('#jump')
            check('jump-to-latest reaches the bottom', devtools.js('Math.abs(previewSnapshot().scrollHeight-previewSnapshot().viewport-previewSnapshot().scrollTop)<2'))
            devtools.click('#history')

            devtools.click('[data-scene="failed"]')
            check('failure visible with details collapsed', devtools.js('document.querySelectorAll("details[open]").length===0 && document.querySelector("[data-key=failure]").textContent.includes("退出码 1")'))
            devtools.capture('failed-100-dark.png')
            devtools.click('[data-scene="approval"]')
            check('approval includes full command before any decision', devtools.js('document.querySelector(".approval-command").textContent.split("\\n").length===11'))
            devtools.capture('approval-100-dark.png')
            devtools.click('[data-decision="deny"]')
            check('refusal is explicit and does not run a command', devtools.js('document.querySelector(".approval-result").textContent.includes("命令未执行")'))

            devtools.click('[data-scene="done"]')
            devtools.js('document.querySelector("#width").value="60";document.querySelector("#width").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.capture('completed-60-dark.png')
            devtools.js('document.querySelector("#theme").value="light";document.querySelector("#theme").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.capture('completed-60-light.png')
            checks = devtools.js('''(() => {
              const transcript=document.querySelector('#transcript');
              return {
                terminalFits:document.querySelector('#terminal').scrollWidth<=document.querySelector('#terminal').clientWidth,
                transcriptFits:transcript.scrollWidth<=transcript.clientWidth,
                summariesOneRow:[...document.querySelectorAll('summary')].every(el=>el.getBoundingClientRect().height===22),
                noEmoji:!(/\\p{Extended_Pictographic}/u.test(document.querySelector('#terminal').textContent)),
                viewportWidth:document.querySelector('#terminal').getBoundingClientRect().width,
                contrast:[...['--fg','--muted','--accent','--error','--warning','--bg']].map(k=>[k,getComputedStyle(document.querySelector('#terminal')).getPropertyValue(k).trim()])
              }
            })()''')
            check('60-column transcript does not overflow horizontally', checks['terminalFits'] and checks['transcriptFits'])
            check('60-column compact command headers stay one row', checks['summariesOneRow'])
            check('60-column terminal still contains no pictographic emoji', checks['noEmoji'])
            devtools.click('[data-scene="edit"]')
            check('edit block has a file path and exact change counts', devtools.js('document.querySelector(".diff-path").textContent === "src/config-reader.ts" && document.querySelector(".diff-stat").textContent === "+2 -1"'))
            check('removed and added code have distinct full-row backgrounds', devtools.js('getComputedStyle(document.querySelector(".diff-line.del")).backgroundColor !== getComputedStyle(document.querySelector(".diff-line.add")).backgroundColor && getComputedStyle(document.querySelector(".diff-line.add")).backgroundColor !== "rgba(0, 0, 0, 0)"'))
            check('word highlights differ from their line backgrounds', devtools.js('[...document.querySelectorAll(".diff-word")].every(el => getComputedStyle(el).backgroundColor !== getComputedStyle(el.closest(".diff-line")).backgroundColor)'))
            check('line coordinates come from the sample patch', devtools.js('[...document.querySelectorAll(".diff-no")].map(el=>el.textContent).join(",") === "20,21,22,22,23,24"'))
            check('60-column diff remains inside transcript width', devtools.js('document.querySelector("#transcript").scrollWidth <= document.querySelector("#transcript").clientWidth'))
            devtools.capture('edit-60-light.png')
            check('removed diff action buttons are absent', devtools.js('!document.querySelector("[data-copy-new]") && !document.querySelector("[data-toggle-patch]") && !document.querySelector(".diff-raw")'))
            check('short diff stays fully visible without a redundant expansion hint', devtools.js('document.querySelector(".diff-block").dataset.foldable === "false" && document.querySelector(".diff-fold-note").hidden'))
            devtools.js('document.querySelector("#theme").value="dark";document.querySelector("#theme").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.capture('edit-60-dark.png')
            devtools.js('document.querySelector("#width").value="100";document.querySelector("#width").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.capture('edit-100-dark.png')
            devtools.click('#diff-status')
            check('unapplied edit is labelled as a preview', devtools.js('document.querySelector(".diff-state").textContent.includes("尚未应用") && document.querySelector(".answer .lead").textContent.includes("准备")'))
            devtools.capture('edit-pending-100-dark.png')
            devtools.click('#diff-status')
            devtools.js('document.querySelector("#diff-example").value="long";document.querySelector("#diff-example").dispatchEvent(new Event("change"))')
            devtools.settle()
            check('long diff is capped to at most eight visual rows', devtools.js('document.querySelector(".diff-body").getBoundingClientRect().height <= 176 && Number(document.querySelector(".diff-body").dataset.hiddenRows)>0'))
            check('long diff preserves all-file change totals', devtools.js('document.querySelector(".diff-stat").textContent === "+14 -13"'))
            check('long diff has a count and whole-block affordance', devtools.js('document.querySelector(".diff-block").dataset.foldable==="true" && document.querySelector(".diff-fold-note").textContent.includes("点击此块展开")'))
            devtools.capture('edit-long-100-dark.png')
            heading_before=devtools.js('document.querySelector(".diff-head").getBoundingClientRect().top')
            devtools.click('.diff-line.del .diff-code')
            check('clicking code content reveals the same styled diff', devtools.js('document.querySelector(".diff-body").getBoundingClientRect().height>176 && document.querySelector(".diff-block").getAttribute("aria-expanded")==="true" && !document.querySelector(".diff-raw")'))
            heading_after=devtools.js('document.querySelector(".diff-head").getBoundingClientRect().top')
            check('expanding a diff preserves its header position', abs(heading_before-heading_after)<1)
            devtools.capture('edit-long-expanded-100-dark.png')
            devtools.js('''(() => { const r=document.createRange();r.selectNodeContents(document.querySelector('.diff-line.add .diff-code'));const s=window.getSelection();s.removeAllRanges();s.addRange(r); })()''')
            devtools.click('.diff-line.add .diff-code')
            check('selection inside a code diff does not trigger folding', devtools.js('document.querySelector(".diff-block").getAttribute("aria-expanded")==="true"'))
            devtools.js('window.getSelection().removeAllRanges()')
            devtools.click('.diff-line.add .diff-code')
            check('clicking code content again collapses the diff', devtools.js('document.querySelector(".diff-body").getBoundingClientRect().height<=176 && document.querySelector(".diff-block").getAttribute("aria-expanded")==="false"'))
            rapid=devtools.js('''(() => { const states=[];for(let i=0;i<6;i++){document.querySelector('.diff-line.del .diff-code').click();states.push(document.querySelector('.diff-block').getAttribute('aria-expanded'));}return states; })()''')
            check('rapid whole-block toggles alternate without delayed state', rapid==['true','false','true','false','true','false'])
            devtools.js('document.querySelector("#width").value="60";document.querySelector("#width").dispatchEvent(new Event("change"))')
            devtools.settle()
            check('long diff remains capped at 60 columns', devtools.js('document.querySelector(".diff-body").getBoundingClientRect().height<=176 && document.querySelector("#transcript").scrollWidth<=document.querySelector("#transcript").clientWidth'))
            devtools.capture('edit-long-60-dark.png')
            devtools.js('document.querySelector("#diff-example").value="wide";document.querySelector("#diff-example").dispatchEvent(new Event("change"))')
            devtools.settle()
            check('one huge source line is capped by visual rows too', devtools.js('document.querySelector(".diff-body").getBoundingClientRect().height<=176 && Number(document.querySelector(".diff-body").dataset.hiddenRows)>0 && document.querySelector("#transcript").scrollWidth<=document.querySelector("#transcript").clientWidth'))
            devtools.capture('edit-long-line-60-dark.png')
            devtools.click('.diff-line.add .diff-code')
            check('explicit expansion preserves the tail of a long line', devtools.js('document.querySelector(".diff-body").textContent.includes("feature_48") && document.querySelector(".diff-body").getBoundingClientRect().height>176'))
            check('edit specimen contains no pictographic emoji', devtools.js('!(/\\p{Extended_Pictographic}/u.test(document.querySelector("#terminal").textContent))'))
            devtools.js('document.querySelector("#width").value="100";document.querySelector("#width").dispatchEvent(new Event("change"))')
            devtools.click('[data-scene="subagents"]')
            check('the parent dock contains only four child entries and overflow, never a main-agent entry', devtools.js('document.querySelectorAll("#subagent-dock [data-agent]").length===5 && !document.querySelector("[data-agent=main]") && !document.querySelector("#terminal").textContent.includes("主代理") && previewSnapshot().subagents.visible.length===4 && previewSnapshot().subagents.extra===2'))
            check('all dock rows fit their container without clipping', devtools.js('(() => {const dock=document.querySelector("#subagent-dock").getBoundingClientRect();return [...document.querySelectorAll("#subagent-dock [data-agent]")].every(row=>row.getBoundingClientRect().bottom<=dock.bottom)})()'))
            check('dock retains the existing text and color states', devtools.js('(() => {const classes=[...document.querySelectorAll("#subagent-dock .subagent-light")].map(el=>el.className).join(" ");return classes.includes("error")&&classes.includes("cancelled")&&classes.includes("completed")&&classes.includes("running")})()'))
            check('subagent mode focuses a real prompt', devtools.js('document.activeElement.id==="agent-prompt"'))
            devtools.capture('subagent-dock-100-dark.png')
            devtools.mouse_click('#subagent-dock [data-agent="find"]')
            check('a physical mouse click enters a subordinate view and hides the parent task list', devtools.js('previewSnapshot().subagents.viewed==="find" && document.querySelector("#transcript").hidden && !document.querySelector("#subagent-transcript").hidden && document.querySelector("#subagent-dock").hidden && !document.querySelector("#subagent-dock [data-agent]") && document.querySelector("#subagent-context").textContent.includes("子代理")'))
            check('the obsolete Peek and Attach overlays are absent', devtools.js('!document.querySelector(".subagent-peek,.subagent-fullscreen") && !document.querySelector("#subagent-transcript").textContent.includes("Attach")'))
            check('child view stays inside the terminal rather than covering the browser', devtools.js('(() => {const parent=document.querySelector("#terminal").getBoundingClientRect(),child=document.querySelector("#subagent-transcript").getBoundingClientRect();return child.left>=parent.left && child.right<=parent.right && child.top>=parent.top && child.bottom<=parent.bottom && document.elementFromPoint(20,20).closest("#terminal")===null})()'))
            check('the shared prompt is addressed to the viewed subagent', devtools.js('document.activeElement.id==="agent-prompt" && document.querySelector("#agent-prompt").getAttribute("aria-label").includes("查找配置依赖") && document.querySelectorAll("#agent-composer").length===1'))
            devtools.call('Input.insertText', {'text':'请优先检查读取边界。'})
            devtools.press('ArrowDown')
            check('arrow keys do not leave a non-empty prompt', devtools.js('document.activeElement.id==="agent-prompt" && previewSnapshot().subagents.focus===null'))
            devtools.press('Escape')
            check('one Escape exits the child with its unsent draft intact and restores the entry focus', devtools.js('previewSnapshot().subagents.viewed==="main" && document.querySelector("#agent-prompt").value==="" && previewSnapshot().subagents.drafts.find==="请优先检查读取边界。" && document.activeElement.dataset.agent==="find"'))
            check('exiting the child does not stop its running task', devtools.js('document.querySelector("#subagent-dock [data-agent=find] .subagent-status").textContent.includes("执行中")'))
            devtools.mouse_click('#agent-prompt')
            devtools.call('Input.insertText', {'text':'原对话草稿'})
            devtools.js('document.querySelector("#agent-prompt").setSelectionRange(2,4)')
            devtools.mouse_click('#subagent-dock [data-agent="find"]')
            check('returning to the subagent restores its draft', devtools.js('document.querySelector("#agent-prompt").value==="请优先检查读取边界。"'))
            devtools.press('Enter')
            check('Enter appends a local message only to the viewed subagent', devtools.js('previewSnapshot().subagents.messages.find.length===2 && !previewSnapshot().subagents.messages.main && document.querySelector("#agent-prompt").value==="" && document.querySelector("#subagent-transcript [data-local-message=user]").textContent.includes("请优先检查读取边界。")'))
            devtools.press('Enter')
            check('empty Enter cannot submit a duplicate message', devtools.js('previewSnapshot().subagents.messages.find.length===2'))
            devtools.call('Input.insertText', {'text':'第一行'})
            devtools.press('Enter', shift=True)
            devtools.call('Input.insertText', {'text':'第二行'})
            check('Shift+Enter creates a newline without sending', devtools.js('document.querySelector("#agent-prompt").value==="第一行\\n第二行" && previewSnapshot().subagents.messages.find.length===2'))
            devtools.js('document.querySelector("#agent-prompt").dispatchEvent(new CompositionEvent("compositionstart",{bubbles:true}))')
            devtools.press('Enter')
            check('IME confirmation does not send a message', devtools.js('previewSnapshot().subagents.messages.find.length===2'))
            devtools.js('document.querySelector("#agent-prompt").dispatchEvent(new CompositionEvent("compositionend",{bubbles:true}))')
            devtools.mouse_click('#agent-composer button[type=submit]')
            check('the send button delivers a multiline local message', devtools.js('previewSnapshot().subagents.messages.find.length===4 && previewSnapshot().subagents.messages.find[2].text==="第一行\\n第二行"'))
            devtools.call('Input.insertText', {'text':'<img src=x onerror="window.specimenInjected=true">'})
            devtools.press('Enter')
            check('message text is escaped instead of becoming HTML', devtools.js('!window.specimenInjected && !document.querySelector("#subagent-transcript img") && document.querySelector("#subagent-transcript").textContent.includes("<img src=x")'))
            devtools.capture('subagent-conversation-100-dark.png')
            devtools.press('ArrowLeft')
            check('left arrow on an empty child prompt restores the parent draft and caret selection', devtools.js('previewSnapshot().subagents.viewed==="main" && document.querySelector("#agent-prompt").value==="原对话草稿" && document.querySelector("#agent-prompt").selectionStart===2 && document.querySelector("#agent-prompt").selectionEnd===4'))
            devtools.mouse_click('#subagent-dock [data-agent="find"]')
            devtools.press('Escape')
            check('Escape from the child prompt returns to its parent entry', devtools.js('previewSnapshot().subagents.viewed==="main" && document.activeElement.dataset.agent==="find"'))
            for selector in ['#subagent-transcript','#subagent-transcript summary','#agent-composer button[type=submit]']:
                devtools.mouse_click('#subagent-dock [data-agent="find"]')
                devtools.js(f'document.querySelector({json.dumps(selector)}).focus()')
                devtools.press('Escape')
                check(f'one Escape exits the child from {selector}', devtools.js('previewSnapshot().subagents.viewed==="main" && document.activeElement.dataset.agent==="find"'))
            devtools.js('document.querySelector("#agent-prompt").value="";document.querySelector("#agent-prompt").dispatchEvent(new Event("input"));document.querySelector("#agent-prompt").focus()')
            devtools.press('ArrowDown')
            check('empty parent prompt arrows select a child without entering it', devtools.js('previewSnapshot().subagents.focus==="find" && previewSnapshot().subagents.viewed==="main"'))
            devtools.press('ArrowRight')
            check('right arrow on a dock row directly opens its conversation', devtools.js('previewSnapshot().subagents.viewed==="find" && document.activeElement.id==="agent-prompt"'))
            devtools.press('ArrowUp')
            check('empty child prompt cannot focus its hidden sibling list', devtools.js('previewSnapshot().subagents.viewed==="find" && document.activeElement.id==="agent-prompt" && previewSnapshot().subagents.focus===null'))
            devtools.press('Escape')
            check('Escape exits the child immediately instead of first returning to an internal prompt', devtools.js('previewSnapshot().subagents.viewed==="main" && document.activeElement.dataset.agent==="find"'))
            devtools.click('[data-scene="subagents"]')
            devtools.press('ArrowDown')
            devtools.press('Enter')
            check('Enter on a dock row uses the same direct route as a click', devtools.js('previewSnapshot().subagents.viewed==="find"'))
            devtools.click('[data-scene="subagents"]')
            devtools.press('ArrowUp')
            devtools.press('Enter')
            check('overflow opens the all-agent list with real keyboard focus', devtools.js('previewSnapshot().subagents.dashboard && document.querySelector("#subagent-dashboard").contains(document.activeElement) && document.querySelectorAll("#subagent-dashboard [data-agent]").length===6 && !document.querySelector("#subagent-dashboard [data-agent=main]")'))
            check('the all-agent list is confined to the conversation viewport', devtools.js('(() => {const list=document.querySelector("#subagent-dashboard").getBoundingClientRect(),viewport=document.querySelector(".viewport-shell").getBoundingClientRect();return list.top>=viewport.top&&list.bottom<=viewport.bottom&&list.left>=viewport.left&&list.right<=viewport.right})()'))
            devtools.capture('subagent-list-100-dark.png')
            devtools.press('ArrowDown')
            devtools.press('ArrowRight')
            check('keyboard navigation in the all-child list enters the selected child', devtools.js('!previewSnapshot().subagents.dashboard && previewSnapshot().subagents.viewed==="failed" && document.querySelector("#subagent-dock").hidden'))
            devtools.press('Escape')
            check('exiting a child opened from the list returns to that list and selection', devtools.js('previewSnapshot().subagents.viewed==="main" && previewSnapshot().subagents.dashboard && document.activeElement.dataset.agent==="failed"'))
            devtools.press('Escape')
            check('a second Escape closes the list to reveal the parent conversation', devtools.js('!previewSnapshot().subagents.dashboard && previewSnapshot().subagents.viewed==="main"'))
            devtools.click('[data-scene="subagents"]')
            devtools.mouse_click('#subagent-dock [data-agent="overflow"]')
            devtools.mouse_click('#subagent-dashboard [data-agent="more-2"]')
            check('a mouse click in the all-agent list opens the requested child', devtools.js('previewSnapshot().subagents.viewed==="more-2" && !previewSnapshot().subagents.dashboard'))
            check('a child reached through overflow has no parallel parent or sibling selector', devtools.js('document.querySelector("#subagent-dock").hidden && document.querySelector("#subagent-dashboard").hidden && !document.querySelector("#subagent-dock [data-agent]") && !document.querySelector("[data-agent=main]")'))
            devtools.mouse_click('#subagent-back')
            check('the header exit button restores the originating list and child selection', devtools.js('previewSnapshot().subagents.viewed==="main" && previewSnapshot().subagents.dashboard && document.activeElement.dataset.agent==="more-2"'))
            devtools.press('Escape')
            devtools.click('[data-scene="subagents"]')
            devtools.mouse_click('#subagent-dock [data-agent="overflow"]')
            devtools.press('Escape')
            check('Escape closes the all-agent list without changing the viewed conversation', devtools.js('!previewSnapshot().subagents.dashboard && previewSnapshot().subagents.viewed==="main" && document.activeElement.id==="agent-prompt"'))
            devtools.click('[data-scene="subagents"]')
            devtools.mouse_click('#subagent-dock [data-agent="overflow"]')
            devtools.js('document.querySelector("[data-close-dashboard]").focus()')
            devtools.press('Enter')
            check('the all-agent close button also works with Enter', devtools.js('!previewSnapshot().subagents.dashboard && document.activeElement.id==="agent-prompt"'))
            devtools.click('#history')
            devtools.js('document.querySelector("#transcript").scrollTop=130')
            devtools.mouse_click('#subagent-dock [data-agent="find"]')
            devtools.js('document.querySelector("#subagent-transcript").scrollTop=90')
            devtools.press('Escape')
            check('main conversation restores its reading position after a child visit', devtools.js('Math.abs(document.querySelector("#transcript").scrollTop-130)<2'))
            devtools.mouse_click('#subagent-dock [data-agent="find"]')
            check('each subagent restores its own reading position', devtools.js('Math.abs(document.querySelector("#subagent-transcript").scrollTop-90)<2'))
            devtools.js('document.querySelector("#subagent-transcript").focus()')
            devtools.press('ArrowDown')
            check('child transcript arrow scrolling moves the visible conversation', devtools.js('Math.abs(document.querySelector("#subagent-transcript").scrollTop-112)<2'))
            devtools.press('PageDown')
            check('PageDown scrolls the child rather than the hidden main transcript', devtools.js('document.querySelector("#subagent-transcript").scrollTop>112 && previewSnapshot().subagents.scrolls.main===130'))
            devtools.mouse_click('#jump')
            check('jump-to-latest targets the visible child transcript', devtools.js('(() => {const area=document.querySelector("#subagent-transcript");return Math.abs(area.scrollHeight-area.scrollTop-area.clientHeight)<2})()'))
            devtools.mouse_click('#subagent-back')
            devtools.mouse_click('#agent-prompt')
            devtools.call('Input.insertText', {'text':'原对话补充'})
            devtools.press('Enter')
            check('sending to main preserves loaded history and does not write to a child', devtools.js('previewSnapshot().subagents.messages.main.length===2 && !previewSnapshot().subagents.messages.find && document.querySelectorAll("#transcript .history-turn").length===10'))
            devtools.click('#history')
            devtools.click('[data-scene="subagents"]')
            devtools.mouse_click('#subagent-dock [data-agent="failed"]')
            devtools.call('Input.insertText', {'text':'保留这份草稿'})
            devtools.js('new Promise(resolve=>setTimeout(resolve,1700))')
            devtools.settle()
            check('settlement cleanup does not close the currently viewed child or steal its draft', devtools.js('previewSnapshot().subagents.viewed==="failed" && !previewSnapshot().subagents.visible.includes("failed") && document.querySelector("#subagent-dock").hidden && document.querySelector("#agent-prompt").value==="保留这份草稿" && document.activeElement.id==="agent-prompt"'))
            devtools.press('Escape')
            check('success, failure and cancellation all leave the dock after the grace period', devtools.js('["done","failed","cancelled"].every(id=>previewSnapshot().subagents.dismissed.includes(id)&&!document.querySelector(`#subagent-dock [data-agent="${id}"]`))'))
            check('completed facts remain accessible from the main conversation', devtools.js('["done","failed","cancelled"].every(id=>document.querySelector(`#transcript [data-agent="${id}"]`)) && document.querySelector("#transcript").textContent.includes("退出码为 1")'))
            devtools.mouse_click('#transcript [data-agent="done"]')
            check('a completed result can still open its conversation after dock cleanup', devtools.js('previewSnapshot().subagents.viewed==="done" && document.querySelector("#subagent-transcript").textContent.includes("12 项")'))
            for width, theme in [(100,'dark'),(60,'dark'),(60,'light')]:
                devtools.js(f'document.querySelector("#width").value="{width}";document.querySelector("#width").dispatchEvent(new Event("change"));document.querySelector("#theme").value="{theme}";document.querySelector("#theme").dispatchEvent(new Event("change"))')
                devtools.settle()
                check(f'child detail and input fit {width} columns in {theme} without a sibling dock', devtools.js('(() => {const terminal=document.querySelector("#terminal"),bounds=terminal.getBoundingClientRect();return terminal.scrollWidth<=terminal.clientWidth && document.querySelector("#subagent-dock").hidden && [...document.querySelectorAll("#agent-composer,#subagent-context,#subagent-transcript")].every(el=>el.getBoundingClientRect().left>=bounds.left&&el.getBoundingClientRect().right<=bounds.right&&el.getBoundingClientRect().bottom<=bounds.bottom)})()'))
                devtools.capture(f'subagent-conversation-{width}-{theme}.png')
            devtools.call('Emulation.setDeviceMetricsOverride', {'width':390,'height':844,'deviceScaleFactor':1,'mobile':True})
            devtools.settle()
            devtools.js('document.querySelector("#terminal").scrollIntoView({block:"start"})')
            devtools.settle()
            check('mobile viewport does not acquire document-level horizontal overflow', devtools.js('document.documentElement.scrollWidth<=innerWidth'))
            check('mobile child transcript and prompt remain inside the terminal without peer navigation', devtools.js('(() => {const terminal=document.querySelector("#terminal").getBoundingClientRect(),input=document.querySelector("#agent-prompt").getBoundingClientRect();return input.right<=terminal.right&&input.bottom<=terminal.bottom&&document.querySelector("#subagent-dock").hidden})()'))
            check('mobile terminal controls are visible after scrolling to the specimen', devtools.js('document.querySelector("#agent-composer").getBoundingClientRect().bottom<=innerHeight && document.querySelector("#agent-prompt").getBoundingClientRect().top>=0'))
            devtools.capture('subagent-conversation-mobile.png')
            devtools.call('Emulation.setDeviceMetricsOverride', {'width':1440,'height':1060,'deviceScaleFactor':1,'mobile':False})
            devtools.js('document.querySelector("#width").value="100";document.querySelector("#width").dispatchEvent(new Event("change"));document.querySelector("#theme").value="dark";document.querySelector("#theme").dispatchEvent(new Event("change"))')
            devtools.click('[data-scene="questions"]')
            check('leaving subagent mode removes its input and restores the question focus', devtools.js('document.querySelector("#agent-composer").hidden && document.querySelector("#subagent-dock").hidden && document.querySelector("#subagent-transcript").hidden && !document.querySelector("#transcript").hidden && document.activeElement.classList.contains("choice-panel")'))

            devtools.js('document.querySelector("#width").value="100";document.querySelector("#width").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.js('document.querySelector(".choice-panel").focus()')
            check('question focus does not preselect an answer', devtools.js('previewSnapshot().choices.states[0].selected.length===0 && document.querySelector("[data-question-option][data-focused=true]").getAttribute("aria-selected")==="false"'))
            devtools.capture('question-single-100-dark.png')
            devtools.press('ArrowDown')
            check('arrow navigation changes focus without answering', devtools.js('previewSnapshot().choices.pending && previewSnapshot().choices.states[0].focus===1 && previewSnapshot().choices.states[0].selected.length===0'))
            devtools.press('1')
            check('single-select digit chooses and finishes directly', devtools.js('previewSnapshot().choices.result.kind==="answered" && previewSnapshot().choices.result.answers[0].selected[0]==="聚焦回归（推荐）"'))
            devtools.js('document.querySelector("#question-mode").value="multi";document.querySelector("#question-mode").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.press(' ')
            devtools.press('ArrowDown')
            devtools.press(' ')
            devtools.capture('question-multi-100-dark.png')
            devtools.press('ArrowDown')
            devtools.press('Enter')
            check('multi-select Enter submits checked items without adding the focused item', devtools.js('JSON.stringify(previewSnapshot().choices.result.answers[0].selected)===JSON.stringify(["普通宽度","窄终端"])'))
            devtools.click('#question-reset')
            devtools.press('Enter')
            check('empty multi-select remains open with a visible error', devtools.js('previewSnapshot().choices.pending && Boolean(document.querySelector(".choice-error"))'))
            devtools.js('document.querySelector("#question-mode").value="single";document.querySelector("#question-mode").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.press('4')
            devtools.press('2')
            devtools.press(' ')
            devtools.press('n')
            devtools.call('Input.insertText',{'text':' 自定义说明'})
            devtools.press('Enter')
            check('Other accepts digits, spaces, letters and pasted text as input', devtools.js('previewSnapshot().choices.result.answers[0].custom==="2 n 自定义说明" && previewSnapshot().choices.result.answers[0].selected.length===0'))
            devtools.js('document.querySelector("#question-mode").value="batch";document.querySelector("#question-mode").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.press('1')
            devtools.press(' ')
            devtools.click('[data-question-tab="0"]')
            devtools.press('2')
            check('returning to change one question preserves another question selection', devtools.js('previewSnapshot().choices.states[0].selected[0]===1 && previewSnapshot().choices.states[1].selected[0]===0'))
            devtools.press('Enter')
            check('multi-question answers wait for the final submit screen', devtools.js('previewSnapshot().choices.pending && previewSnapshot().choices.index===2'))
            devtools.capture('question-submit-100-dark.png')
            devtools.press('Enter')
            check('final submit returns both questions with their original ids', devtools.js('previewSnapshot().choices.result.answers.map(a=>a.id).join(",")==="checks,surfaces"'))
            devtools.click('#question-reset')
            devtools.press('Tab')
            devtools.press('Tab')
            devtools.press('Enter')
            check('skipped questions cannot silently submit', devtools.js('previewSnapshot().choices.pending && previewSnapshot().choices.index===0 && Boolean(document.querySelector(".choice-error"))'))
            devtools.press('Escape')
            check('Escape cancels without fabricating answered results', devtools.js('previewSnapshot().choices.result.kind==="cancelled"'))
            devtools.js('document.querySelector("#question-mode").value="multi";document.querySelector("#question-mode").dispatchEvent(new Event("change"));document.querySelector("#width").value="60";document.querySelector("#width").dispatchEvent(new Event("change"));document.querySelector("#theme").value="light";document.querySelector("#theme").dispatchEvent(new Event("change"))')
            devtools.settle()
            devtools.js('document.querySelector(".choice-panel").focus()')
            devtools.press(' ')
            devtools.capture('question-multi-60-light.png')
            check('question card fits 60 columns and keeps the footer visible', devtools.js('document.querySelector("#choice-host").scrollWidth<=document.querySelector("#choice-host").clientWidth && document.querySelector(".choice-footer").getBoundingClientRect().bottom<=document.querySelector("#terminal").getBoundingClientRect().bottom'))
            check('question card contains no pictographic decoration', devtools.js('!(/\\p{Extended_Pictographic}/u.test(document.querySelector("#choice-host").textContent))'))
            check('question footer also omits model, context number and Ctrl+O hint', devtools.js('!document.querySelector(".status").textContent.includes("DeepSeek") && !document.querySelector(".status").textContent.includes("上下文") && !document.querySelector(".status").textContent.includes("Ctrl+O")'))
            check('question state keeps the same input-border context signal', devtools.js('parseFloat(getComputedStyle(document.querySelector(".composer"),"::before").width)>0'))
            check('no remote requests while using the offline specimen', not any(url.startswith(('http://','https://','ws://','wss://')) for url in devtools.requests))
            check('no uncaught browser exceptions', not devtools.errors)
            summary={'checks':results,'narrow':checks,'browserErrors':devtools.errors,'scope':'Browser layout and specimen interactions only; no terminal performance claim.'}
            (ROOT/'verification.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
            print(json.dumps(summary,ensure_ascii=False,indent=2))
        finally:
            if devtools:
                devtools.socket.close()
            process.terminate()
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)
if any(not result['passed'] for result in results):
    raise SystemExit(1)
