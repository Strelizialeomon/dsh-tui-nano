// Offline interaction specimen. All messages and task states stay in this page.
(() => {
  'use strict'

  const fixtures = () => [
    {id:'find',name:'查找配置依赖',status:'running',statusText:'执行中',prompt:'查找上下文容量线的数据来源，核对配置依赖。',tool:'rg -n contextWindow src/',lines:['已找到 channelContextOccupancy 的读取入口。','正在整理依赖关系与调用入口。']},
    {id:'failed',name:'修复读取边界',status:'failed',statusText:'错误',prompt:'检查空配置与损坏配置的读取边界。',tool:'node scripts/verify-config-reader.mjs',lines:['测试数据文件不存在。','检查退出码为 1，尚不能确认修复通过。']},
    {id:'cancelled',name:'检查主题回归',status:'cancelled',statusText:'终止',prompt:'检查深色和浅色主题的状态呈现。',tool:'node scripts/verify-themes.mjs',lines:['用户中止了这次检查。','主题回归没有完成。']},
    {id:'done',name:'核对样式回归',status:'completed',statusText:'完成',prompt:'核对样式相关的回归样例。',tool:'node scripts/verify-styles.mjs',lines:['已核对 12 项样式检查。','本组演示检查全部通过。']},
    {id:'more-1',name:'验证窄终端布局',status:'running',statusText:'执行中',prompt:'检查 60 列终端下的布局。',tool:'DSH_TEST_COLUMNS=60 node scripts/verify-layout.mjs',lines:['正在检查差异块和选项卡。','核对长路径、换行与输入框位置。']},
    {id:'more-2',name:'跑完整构建',status:'running',statusText:'执行中',prompt:'运行完整构建并收集真实退出状态。',tool:'pnpm build',lines:['等待构建资源。','构建尚未完成，没有可报告的通过结果。']},
  ]
  const settled = row => ['completed','failed','cancelled'].includes(row.status)
  const statusClass = row => row.status === 'failed' ? 'error' : row.status

  class SubagentPreview {
    constructor({terminal,mainTranscript,cells,onChange}) {
      this.terminal = terminal
      this.mainTranscript = mainTranscript
      this.cells = cells
      this.onChange = onChange
      this.childTranscript = terminal.querySelector('#subagent-transcript')
      this.dashboard = terminal.querySelector('#subagent-dashboard')
      this.dock = terminal.querySelector('#subagent-dock')
      this.heading = terminal.querySelector('#subagent-context')
      this.form = terminal.querySelector('#agent-composer')
      this.input = terminal.querySelector('#agent-prompt')
      this.active = false
      this.reset()
      this.dock.addEventListener('click', event => this.openFromRow(event))
      this.dashboard.addEventListener('click', event => {
        if (event.target.closest('[data-close-dashboard]')) this.closeDashboard()
        else this.openFromRow(event)
      })
      this.mainTranscript.addEventListener('click', event => this.openFromRow(event))
      this.dock.addEventListener('focusin', event => {
        const row = event.target.closest('[data-agent]')
        if (row) { this.focus = row.dataset.agent; this.paintFocus() }
      })
      this.dashboard.addEventListener('focusin', event => {
        const row = event.target.closest('[data-agent]')
        if (row) { this.dashboardFocus = row.dataset.agent; this.paintFocus() }
      })
      terminal.querySelector('#subagent-back').addEventListener('click', () => this.exit())
      this.form.addEventListener('submit', event => { event.preventDefault(); this.send() })
      this.input.addEventListener('input', () => {
        this.drafts.set(this.viewed, this.input.value)
        this.resizeInput()
      })
      this.input.addEventListener('compositionstart', () => { this.composing = true })
      this.input.addEventListener('compositionend', () => { this.composing = false })
      this.input.addEventListener('focus', () => { this.focus = null; this.paintFocus() })
      this.childTranscript.addEventListener('scroll', () => this.onChange())
      this.childTranscript.addEventListener('toggle', () => this.onChange(), true)
    }
    reset() {
      clearTimeout(this.completionTimer)
      this.completionTimer = null
      this.active = false
      this.rows = fixtures()
      this.viewed = 'main'
      this.focus = null
      this.dashboardOpen = false
      this.dashboardFocus = this.rows[0].id
      this.returnTo = null
      this.listScroll = 0
      this.carets = new Map()
      this.dismissed = new Set()
      this.drafts = new Map()
      this.messages = new Map()
      this.scrolls = new Map()
      this.composing = false
      this.long = false
      this.childTranscript.innerHTML = ''
    }
    visibleRows() {
      const rows = this.rows.filter(row => !this.dismissed.has(row.id))
      return {visible:rows.slice(0,4),extra:Math.max(0,rows.length - 4)}
    }
    dockIds() {
      const {visible,extra} = this.visibleRows()
      return [...visible.map(row => row.id),...(extra > 0 ? ['overflow'] : [])]
    }
    activeTranscript() { return this.active && this.viewed !== 'main' ? this.childTranscript : this.mainTranscript }
    rowButton(id,name,status = '',className = '') {
      const row = this.rows.find(item => item.id === id)
      return `<button type="button" class="subagent-row ${className}" data-agent="${id}" title="${name}"><span class="subagent-pointer" aria-hidden="true"> </span><span class="subagent-light ${row ? statusClass(row) : ''}" aria-hidden="true">●</span><span class="subagent-name">${this.cells(name)}</span><span class="subagent-status">${this.cells(status)}</span></button>`
    }
    paintFocus() {
      for (const host of [this.dock,this.dashboard]) {
        const id = host === this.dashboard ? this.dashboardFocus : this.focus
        host.querySelectorAll('[data-agent]').forEach(row => {
          row.dataset.focused = String(row.dataset.agent === id && host.contains(document.activeElement))
          row.querySelector('.subagent-pointer').textContent = row.dataset.focused === 'true' ? '›' : ' '
        })
      }
    }
    renderDock() {
      const {visible,extra} = this.visibleRows()
      const focused = this.dock.contains(document.activeElement)
      const previous = this.focus
      this.dock.hidden = !this.active || this.viewed !== 'main' || this.dockIds().length === 0
      if (this.dock.hidden) { this.dock.innerHTML = ''; return }
      this.dock.innerHTML = visible.map(row => this.rowButton(row.id,row.name,row.statusText)).join('')
        + (extra > 0 ? this.rowButton('overflow',`还有 ${extra} 个子代理`,'','subagent-more') : '')
      if (focused) {
        this.focus = this.dockIds().includes(previous) ? previous : this.dockIds()[0]
        this.focusRow(this.dock,this.focus)
      }
      this.paintFocus()
    }
    renderDashboard() {
      this.dashboard.hidden = !this.dashboardOpen
      this.mainTranscript.inert = this.dashboardOpen
      this.childTranscript.inert = this.dashboardOpen
      this.form.inert = this.dashboardOpen
      if (!this.dashboardOpen) return
      this.dashboard.innerHTML = `<div class="agent-list-head"><strong>${this.cells('全部子代理')}</strong><button type="button" class="agent-icon" data-close-dashboard aria-label="关闭子代理列表" title="关闭子代理列表">×</button></div><div class="agent-list-body">${this.rows.map(row => this.rowButton(row.id,row.name,row.statusText)).join('')}</div>`
      this.paintFocus()
    }
    mainTranscriptHtml() {
      return `<section class="turn" data-key="subagent-main"><div class="term-row user">${this.cells('> 检查配置读取、界面样式和窄终端布局。')}</div><p>${this.cells('已分派检查。配置依赖、窄终端布局和完整构建仍在进行，其他结果如下。')}</p><div class="agent-results">${this.rows.filter(settled).map(row => `<div class="agent-result"><button type="button" class="agent-result-link" data-agent="${row.id}">${this.cells(row.name)}</button><span class="subagent-status ${statusClass(row)}">${this.cells(row.statusText)}</span><div class="muted">${this.cells(row.lines.join(' '))}</div></div>`).join('')}</div>${this.messageHtml('main')}</section>`
    }
    messageHtml(id,messages = this.messages.get(id) ?? []) {
      return messages.map(message => `<div class="agent-message ${message.role}" data-local-message="${message.role}">${this.cells(message.role === 'user' ? `> ${message.text}` : message.text)}</div>`).join('')
    }
    renderConversation() {
      const row = this.rows.find(item => item.id === this.viewed)
      const name = row?.name ?? '当前对话'
      this.mainTranscript.hidden = Boolean(row)
      this.childTranscript.hidden = !row
      this.heading.hidden = !row
      this.heading.querySelector('#subagent-title').textContent = name
      this.heading.querySelector('#subagent-title').title = name
      this.heading.querySelector('#subagent-view-status').textContent = row?.statusText ?? ''
      if (row) {
        const history = this.long ? Array.from({length:12},(_,index) => `<div class="history-turn"><div class="term-row user">${this.cells(`> 第 ${index + 1} 轮：核对 ${name}`)}</div><p>${this.cells('已保留这轮检查记录，继续核对输入和返回值。')}</p></div>`).join('') : ''
        const toolState = row.status === 'running' ? '运行中' : row.statusText
        this.childTranscript.innerHTML = history + `<section data-agent-transcript="${row.id}"><div class="term-row user">${this.cells(`> ${row.prompt}`)}</div><p>${this.cells(`我会${row.prompt.replace(/。$/,'')}。`)}</p><details class="process"><summary><span class="disclose" aria-hidden="true"></span><span class="summary-kind">Bash</span><span class="summary-name">${this.cells(row.tool)}</span><span class="summary-meta">${this.cells(toolState)}</span></summary><div class="detail"><pre>${this.cells(row.tool)}</pre><div class="body-section">${this.cells(row.lines.join('\n'))}</div></div></details><div class="agent-output ${statusClass(row)}">${this.cells(row.lines.join('\n'))}</div>${this.messageHtml(row.id)}</section>`
      }
      this.input.value = this.drafts.get(this.viewed) ?? ''
      this.input.placeholder = row ? `给 ${name} 发送消息…` : '继续提问…'
      this.input.setAttribute('aria-label', row ? `给子代理 ${name} 发送消息` : '发送消息')
      this.resizeInput()
      const caret = this.carets.get(this.viewed)
      if (caret) this.input.setSelectionRange(caret.start,caret.end)
    }
    sync(active,long) {
      const entering = active && !this.active
      const changedHistory = this.long !== long
      this.active = active
      this.long = long
      this.form.hidden = !active
      this.terminal.querySelector('#composer-static').hidden = active
      if (!active) {
        clearTimeout(this.completionTimer)
        this.completionTimer = null
        this.mainTranscript.hidden = false
        this.mainTranscript.inert = false
        this.childTranscript.hidden = true
        this.childTranscript.inert = false
        this.form.inert = false
        this.dashboard.hidden = true
        this.heading.hidden = true
        this.renderDock()
        return
      }
      if (entering || changedHistory) this.renderConversation()
      this.renderDock()
      this.renderDashboard()
      if (this.completionTimer === null && this.dismissed.size === 0) {
        this.completionTimer = setTimeout(() => {
          this.rows.filter(settled).forEach(row => this.dismissed.add(row.id))
          this.renderDock()
          this.onChange()
        },1500)
      }
      if (entering) this.input.focus({preventScroll:true})
    }
    openFromRow(event) {
      const row = event.target.closest('[data-agent]')
      if (!row || event.button > 0 || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return
      const selection = window.getSelection()
      if (selection && !selection.isCollapsed) return
      const origin = this.dashboard.contains(row) ? 'list' : this.dock.contains(row) ? 'dock' : 'result'
      this.enter(row.dataset.agent,origin)
    }
    saveView() {
      this.scrolls.set(this.viewed,this.activeTranscript().scrollTop)
      this.drafts.set(this.viewed,this.input.value)
      this.carets.set(this.viewed,{start:this.input.selectionStart,end:this.input.selectionEnd})
    }
    enter(id,origin = 'dock') {
      if (this.viewed !== 'main') return
      if (id === 'overflow') {
        this.dashboardOpen = true
        this.dashboardFocus = this.rows[0].id
        this.renderDashboard()
        this.focusRow(this.dashboard,this.dashboardFocus)
        this.onChange()
        return
      }
      if (!this.rows.some(row => row.id === id)) return
      this.saveView()
      this.returnTo = {origin,id}
      if (origin === 'list') this.listScroll = this.dashboard.querySelector('.agent-list-body').scrollTop
      this.viewed = id
      this.dashboardOpen = false
      this.focus = null
      this.renderConversation()
      this.renderDashboard()
      this.renderDock()
      const target = this.activeTranscript()
      target.scrollTop = this.scrolls.get(id) ?? target.scrollHeight
      this.input.focus({preventScroll:true})
      this.onChange()
    }
    exit() {
      if (this.viewed === 'main') return
      this.saveView()
      const source = this.returnTo
      this.viewed = 'main'
      this.returnTo = null
      this.dashboardOpen = source?.origin === 'list'
      this.renderConversation()
      this.renderDock()
      this.renderDashboard()
      this.mainTranscript.scrollTop = this.scrolls.get('main') ?? 0
      if (this.dashboardOpen) {
        this.dashboardFocus = source.id
        this.dashboard.querySelector('.agent-list-body').scrollTop = this.listScroll
        this.focusRow(this.dashboard,source.id)
      } else {
        const host = source?.origin === 'result' ? this.mainTranscript : this.dock
        const target = [...host.querySelectorAll('[data-agent]')].find(row => row.dataset.agent === source?.id)
        if (target) target.focus({preventScroll:true})
        else this.input.focus({preventScroll:true})
      }
      this.onChange()
    }
    closeDashboard() {
      this.dashboardOpen = false
      this.renderDashboard()
      this.input.focus({preventScroll:true})
      this.onChange()
    }
    focusRow(host,id) {
      const row = [...host.querySelectorAll('[data-agent]')].find(item => item.dataset.agent === id)
      row?.focus({preventScroll:true})
      if (row && host === this.dashboard) {
        const list = host.querySelector('.agent-list-body')
        const bounds = list.getBoundingClientRect()
        const item = row.getBoundingClientRect()
        if (item.top < bounds.top) list.scrollTop -= bounds.top - item.top
        else if (item.bottom > bounds.bottom) list.scrollTop += item.bottom - bounds.bottom
      }
      this.paintFocus()
    }
    resizeInput() {
      this.input.style.height = '22px'
      this.input.style.height = `${Math.min(88,this.input.scrollHeight)}px`
    }
    send() {
      if (!this.active || this.composing) return
      const text = this.input.value.trim()
      if (!text) return
      const id = this.viewed
      const messages = this.messages.get(id) ?? []
      messages.push({role:'user',text},{role:'assistant',text:'已收到这条补充，将据此继续核对；尚无新的检查结果。'})
      this.messages.set(id,messages)
      this.drafts.set(id,'')
      if (id === 'main') this.mainTranscript.querySelector('[data-key="subagent-main"]').insertAdjacentHTML('beforeend',this.messageHtml(id,messages.slice(-2)))
      this.renderConversation()
      this.activeTranscript().scrollTop = this.activeTranscript().scrollHeight
      this.input.focus({preventScroll:true})
      document.querySelector('#action-feedback').textContent = '已在本地样稿中追加对话，未发送网络请求。'
      this.onChange()
    }
    toggleDetails() {
      const details = [...this.activeTranscript().querySelectorAll('details')]
      const open = !details.every(detail => detail.open)
      details.forEach(detail => { detail.open = open })
    }
    handleKey(event) {
      if (!this.active || event.defaultPrevented) return false
      if (event.isComposing || this.composing || event.keyCode === 229) return true
      if (event.ctrlKey || event.metaKey || event.altKey) return false
      const typing = event.target === this.input
      const inDashboard = this.dashboardOpen && this.dashboard.contains(event.target)
      const inDock = this.dock.contains(event.target)
      const key = event.key
      if (this.dashboardOpen && key === 'Escape') {
        event.preventDefault(); this.closeDashboard(); return true
      }
      if (key === 'Escape' && this.viewed !== 'main') {
        event.preventDefault(); this.exit(); return true
      }
      if (inDashboard && event.target.closest('[data-close-dashboard]')) return false
      if (inDashboard || inDock) {
        const host = inDashboard ? this.dashboard : this.dock
        const ids = inDashboard ? this.rows.map(row => row.id) : this.dockIds()
        const current = inDashboard ? this.dashboardFocus : this.focus
        if (key === 'ArrowUp' || key === 'ArrowDown') {
          event.preventDefault()
          const index = ids.indexOf(current)
          const next = ids[(index + (key === 'ArrowDown' ? 1 : -1) + ids.length) % ids.length]
          if (inDashboard) this.dashboardFocus = next
          else this.focus = next
          this.focusRow(host,next)
          return true
        }
        if (key === 'Enter' || key === 'ArrowRight' || key === ' ') {
          event.preventDefault(); this.enter(current,inDashboard ? 'list' : 'dock'); return true
        }
        if (key === 'Escape') {
          event.preventDefault(); this.input.focus({preventScroll:true}); return true
        }
      }
      if (typing) {
        if (key === 'Enter' && !event.shiftKey) { event.preventDefault(); this.send(); return true }
        if ((key === 'ArrowUp' || key === 'ArrowDown') && this.input.value === '' && this.viewed === 'main' && this.dockIds().length > 0) {
          event.preventDefault()
          const ids = this.dockIds()
          this.focus = key === 'ArrowUp' ? ids.at(-1) : ids[0]
          this.focusRow(this.dock,this.focus)
          return true
        }
      }
      if (key === 'ArrowLeft' && typing && this.input.value === '' && this.viewed !== 'main') {
        event.preventDefault(); this.exit(); return true
      }
      if (event.target === this.childTranscript && ['ArrowUp','ArrowDown','PageUp','PageDown','Home','End'].includes(key)) {
        event.preventDefault()
        const target = this.childTranscript
        if (key === 'Home') target.scrollTop = 0
        else if (key === 'End') target.scrollTop = target.scrollHeight
        else target.scrollTop += (key === 'ArrowUp' || key === 'PageUp' ? -1 : 1) * (key.startsWith('Page') ? target.clientHeight / 2 : 22)
        this.onChange()
        return true
      }
      return false
    }
    snapshot() {
      const {visible,extra} = this.visibleRows()
      return {viewed:this.viewed,focus:this.focus,dashboard:this.dashboardOpen,visible:visible.map(row => row.id),extra,dismissed:[...this.dismissed],drafts:Object.fromEntries(this.drafts),scrolls:Object.fromEntries(this.scrolls),messages:Object.fromEntries(this.messages)}
    }
  }
  window.SubagentPreview = SubagentPreview
})()
