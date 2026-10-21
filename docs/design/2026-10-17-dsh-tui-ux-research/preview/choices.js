// Interactive specimen of the Pi-plugin question-card conventions.
// This is local demo state only; no backend requests or permission changes.
(() => {
  const QUESTIONS = [
    { id:'checks',header:'验收方式',question:'这次改动采用哪种验收方式？',multi:false,options:[
      {label:'聚焦回归（推荐）',description:'覆盖本次修改相关的检查，反馈较快。'},
      {label:'完整回归',description:'检查范围更广，需要等待更多脚本完成。'},
      {label:'静态检查',description:'先确认类型和边界，行为验证留待下一步。'},
    ]},
    { id:'surfaces',header:'测试范围',question:'需要覆盖哪些终端场景？',multi:true,options:[
      {label:'普通宽度',description:'检查常规终端里的阅读与输入。'},
      {label:'窄终端',description:'检查换行、折叠和选项可见性。'},
      {label:'流式输出',description:'检查内容增长时的阅读位置。'},
      {label:'带侧栏',description:'检查焦点和滚轮是否落到正确区域。'},
    ]},
  ]
  const esc = text => String(text).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))
  class ChoicePreview {
    constructor({host,cells,onChange}) {
      this.host=host;this.cells=cells;this.onChange=onChange;this.key=null;this.pending=false
      host.addEventListener('click',event=>this.click(event))
      host.addEventListener('input',event=>{
        if(event.target.matches('[data-other-input]')) {
          this.current().draft=event.target.value
          if(!event.target.value.trim()) this.current().other=''
        }
      })
      host.addEventListener('focusin',event=>{
        if(event.target.matches('[data-other-input]') && this.kind==='question' && this.index<this.questions.length) {
          this.current().focus=this.question().options.length
          this.paintFocus()
        }
      })
    }
    hide() { this.key=null;this.pending=false;this.host.hidden=true;this.host.innerHTML='' }
    showQuestions(mode,key) {
      if(this.key===key) {this.paint();return}
      this.key=key;this.kind='question';this.mode=mode;this.index=0;this.error='';this.result=null
      this.questions=mode==='batch'?QUESTIONS:mode==='multi'?[QUESTIONS[1]]:[QUESTIONS[0]]
      this.states=this.questions.map(()=>({selected:[],other:'',draft:'',focus:0}))
      this.pending=true;this.paint();this.focusPanel()
    }
    showApproval(command,key) {
      if(this.key===key) {this.paint();return}
      this.key=key;this.kind='approval';this.command=command;this.approvalFocus=0;this.error='';this.result=null;this.pending=true
      this.paint();this.focusPanel()
    }
    focusPanel() { this.host.querySelector('.choice-panel')?.focus({preventScroll:true}) }
    question() { return this.questions[this.index] }
    current() { return this.states[this.index] }
    valid(item) { return item.selected.length>0 || Boolean(item.other.trim()) }
    answers() { return this.questions.map((question,i)=>({id:question.id,header:question.header,selected:this.states[i].selected.map(j=>question.options[j].label),custom:this.states[i].other})) }
    finish(result) { this.pending=false;this.result=result;this.paint() }
    advance() {
      this.error=''
      if(this.questions.length===1) this.finish({kind:'answered',answers:this.answers()})
      else {this.index=Math.min(this.questions.length,this.index+1);this.paint();this.focusPanel()}
    }
    choose(index) {
      const question=this.question(),state=this.current()
      state.focus=index;this.error=''
      if(index===question.options.length) {this.paint();this.host.querySelector('[data-other-input]')?.focus();return}
      if(question.multi) {
        const at=state.selected.indexOf(index)
        if(at<0) state.selected.push(index)
        else state.selected.splice(at,1)
        this.paint();this.focusPanel()
      } else {state.selected=[index];state.other='';this.advance()}
    }
    confirm() {
      if(this.index===this.questions.length) {
        const missing=this.states.findIndex(state=>!this.valid(state))
        if(missing>=0){this.index=missing;this.error='请先回答这一题。';this.paint();this.focusPanel();return}
        this.finish({kind:'answered',answers:this.answers()});return
      }
      const question=this.question(),state=this.current()
      if(state.focus===question.options.length) {
        const text=state.draft.trim()
        if(!text) {
          state.other=''
          if(question.multi&&state.selected.length){this.advance();return}
          this.error='请填写其他答案，或选择一个选项。';this.paint();this.host.querySelector('[data-other-input]')?.focus();return
        }
        state.other=text
        if(!question.multi)state.selected=[]
        this.advance();return
      }
      if(question.multi) {
        if(!this.valid(state)){this.error='请至少选择一项。';this.paint();return}
        this.advance()
      } else this.choose(state.focus)
    }
    navigate(delta) {
      if(this.questions.length===1)return
      const count=this.questions.length+1
      this.index=(this.index+delta+count)%count;this.error='';this.paint();this.focusPanel()
    }
    move(delta) {
      const state=this.current(),count=this.question().options.length+1
      state.focus=(state.focus+delta+count)%count;this.error='';this.paint()
      if(state.focus===count-1)this.host.querySelector('[data-other-input]')?.focus()
      else this.focusPanel()
    }
    handleKey(event) {
      if(!this.pending)return false
      const typing=event.target.matches('[data-other-input]')
      if(event.key==='Escape') {
        event.preventDefault();this.finish(this.kind==='approval'?{kind:'permission',decision:'deny'}:{kind:'cancelled'});return true
      }
      if(this.kind==='approval') {
        const command=event.target.closest('.approval-command')
        if(command&&['ArrowUp','ArrowDown','PageUp','PageDown'].includes(event.key)) {
          event.preventDefault();const up=event.key==='ArrowUp'||event.key==='PageUp';command.scrollTop+=(up?-1:1)*(event.key.startsWith('Page')?command.clientHeight:22);return true
        }
        if(['ArrowUp','ArrowDown','Tab'].includes(event.key)) {
          event.preventDefault();this.approvalFocus=1-this.approvalFocus;this.paint();this.focusPanel()
        } else if(event.key==='Enter'||event.key==='1'||event.key==='2') {
          event.preventDefault();const index=event.key==='Enter'?this.approvalFocus:Number(event.key)-1
          this.finish({kind:'permission',decision:index===0?'allow':'deny'})
        }
        return true
      }
      if(event.key==='Tab'&&this.questions.length>1){event.preventDefault();this.navigate(event.shiftKey?-1:1);return true}
      if(!typing&&(event.key==='ArrowLeft'||event.key==='ArrowRight')&&this.questions.length>1){event.preventDefault();this.navigate(event.key==='ArrowLeft'?-1:1);return true}
      if(event.key==='Enter'){event.preventDefault();this.confirm();return true}
      if(this.index===this.questions.length) {
        if(event.key==='ArrowUp'||event.key==='ArrowDown'){event.preventDefault();this.host.querySelector('.choice-body').scrollTop+=event.key==='ArrowUp'?-22:22}
        return true
      }
      if(event.key==='ArrowUp'||event.key==='ArrowDown'){event.preventDefault();this.move(event.key==='ArrowUp'?-1:1);return true}
      if(typing)return true // The browser editor owns text, digits, spaces, paste and the caret.
      if(/^[1-9]$/.test(event.key)) {
        const index=Number(event.key)-1
        if(index<=this.question().options.length){event.preventDefault();this.choose(index)}
        return true
      }
      if(event.key===' '){event.preventDefault();this.choose(this.current().focus);return true}
      return true
    }
    click(event) {
      if(!this.pending)return
      const input=event.target.closest('[data-other-input]')
      if(input)return
      const decision=event.target.closest('[data-decision]')
      if(decision){this.finish({kind:'permission',decision:decision.dataset.decision});return}
      const tab=event.target.closest('[data-question-tab]')
      if(tab){this.index=Number(tab.dataset.questionTab);this.error='';this.paint();this.focusPanel();return}
      const option=event.target.closest('[data-question-option]')
      if(option){this.choose(Number(option.dataset.questionOption));return}
      if(event.target.closest('[data-question-confirm]'))this.confirm()
    }
    paintFocus() {
      const state=this.current()
      this.host.querySelectorAll('[data-question-option]').forEach((row,index)=>{
        row.dataset.focused=String(index===state.focus)
        row.querySelector('.choice-pointer').textContent=index===state.focus?'>':' '
      })
    }
    optionRow(index,label,description,{focused=false,selected=false,multi=false,decision}={}) {
      const action=decision?`data-decision="${decision}"`:`data-question-option="${index}"`
      return `<div class="choice-row" role="option" aria-selected="${selected}" data-focused="${focused}" data-selected="${selected}" ${action}>
        <div class="choice-label"><span class="choice-pointer" aria-hidden="true">${focused?'&gt;':' '}</span><span class="choice-number">${index+1}.</span>${multi?`<span class="choice-check">${selected?'[x]':'[ ]'}</span>`:''}<span class="choice-text">${this.cells(label)}</span>${selected&&!multi?'<span class="choice-picked">✓</span>':''}</div>
        ${description?`<div class="choice-description">${this.cells(description)}</div>`:''}
      </div>`
    }
    receipt() {
      if(this.result.kind==='permission')return `<div class="approval-result muted">${this.cells(this.result.decision==='allow'?'已演示“允许一次”。样稿不会执行命令。':'已演示“拒绝”。命令未执行。')}</div>`
      if(this.result.kind==='cancelled')return `<div class="muted">${this.cells('已取消，未提交答案。')}</div>`
      return `<div class="choice-receipt"><strong>${this.cells(`已回答 ${this.result.answers.length} 题`)}</strong>${this.result.answers.map(answer=>`<div class="muted">${this.cells(`· ${answer.header} → ${[...answer.selected,answer.custom].filter(Boolean).join('、')}`)}</div>`).join('')}</div>`
    }
    paint() {
      const previousInput=this.host.querySelector('[data-other-input]')
      const editing=previousInput&&document.activeElement===previousInput
      const caret=editing?previousInput.selectionStart:null
      this.host.hidden=false
      if(this.result){this.host.innerHTML=this.receipt();this.onChange(this);return}
      if(this.kind==='approval') {
        this.host.innerHTML=`<section class="choice-panel" tabindex="0" aria-label="命令授权选项卡">
          <div class="choice-title">${this.cells('是否运行这条检查命令？')}</div>
          <div class="choice-context muted">${this.cells('当前模式要求逐次确认；这是已有授权流程的演示。')}</div>
          <pre class="approval-command" tabindex="0" aria-label="完整命令，可滚动查看">${this.cells(this.command)}</pre>
          <div class="choice-options" role="listbox" aria-label="授权选项">
            ${this.optionRow(0,'允许一次','仅允许本次命令，不更改后续权限。',{focused:this.approvalFocus===0,decision:'allow'})}
            ${this.optionRow(1,'拒绝','不执行本次命令。',{focused:this.approvalFocus===1,decision:'deny'})}
          </div>
          <div class="choice-footer">${this.cells('↑↓ 移动 · Enter 确认 · 1/2 直选 · Esc 拒绝')}</div>
        </section>`
        this.onChange(this);return
      }
      const submit=this.index===this.questions.length
      const tabs=this.questions.length>1?`<div class="choice-tabs" aria-label="题目导航">${this.questions.map((question,index)=>`<button data-question-tab="${index}" class="choice-tab" aria-current="${this.index===index}">${this.valid(this.states[index])?'[x]':'[ ]'} ${this.cells(question.header)}</button>`).join('')}<button data-question-tab="${this.questions.length}" class="choice-tab" aria-current="${submit}">${this.cells('提交')}</button></div>`:''
      let body,title,footer
      if(submit) {
        title='提交这组答案'
        body=this.questions.map((question,index)=>{
          const state=this.states[index],valid=this.valid(state)
          return `<div class="choice-review"><div>${valid?'[x]':'[ ]'} ${this.cells(question.header)}</div><div class="${valid?'muted':'choice-error'}">${this.cells(valid?[...state.selected.map(i=>question.options[i].label),state.other].filter(Boolean).join('、'):'尚未回答')}</div></div>`
        }).join('')+`<button class="choice-confirm" data-question-confirm>${this.cells('提交答案')}</button>`
        footer='Enter 提交 · Tab / ←→ 回看 · Esc 取消'
      } else {
        const question=this.question(),state=this.current()
        title=question.question
        body=`<div class="choice-options" role="listbox" aria-multiselectable="${question.multi}">${question.options.map((option,index)=>this.optionRow(index,option.label,option.description,{focused:state.focus===index,selected:state.selected.includes(index),multi:question.multi})).join('')}
          <div class="choice-row choice-other" data-question-option="${question.options.length}" data-focused="${state.focus===question.options.length}" data-selected="${Boolean(state.other)}" role="option" aria-selected="${Boolean(state.other)}">
            <div class="choice-label"><span class="choice-pointer" aria-hidden="true">${state.focus===question.options.length?'&gt;':' '}</span><span class="choice-number">${question.options.length+1}.</span><span>${this.cells('其他：')}</span><input data-other-input aria-label="其他答案" value="${esc(state.draft)}" placeholder="直接输入" autocomplete="off"></div>
          </div>
        </div>${question.multi?`<button class="choice-confirm" data-question-confirm>${this.cells(this.questions.length===1?'确认已选项':'下一题')}</button>`:''}`
        footer=question.multi?'↑↓ 移动 · 空格勾选 · Enter 确认':'↑↓ 移动 · Enter 选择 · 数字直选'
        if(this.questions.length>1)footer+=' · Tab 切题'
        footer+=' · Esc 取消'
      }
      this.host.innerHTML=`<section class="choice-panel" tabindex="0" aria-label="问卷选项卡">${tabs}<div class="choice-title">${this.cells(title)}</div><div class="choice-body">${body}</div>${this.error?`<div class="choice-error" role="alert">${this.cells(this.error)}</div>`:''}<div class="choice-footer">${this.cells(footer)}</div></section>`
      if(editing&&!submit&&this.current().focus===this.question().options.length) {
        const input=this.host.querySelector('[data-other-input]');input.focus({preventScroll:true});input.setSelectionRange(caret,caret)
      }
      this.host.querySelector('[data-question-option][data-focused="true"]')?.scrollIntoView({block:'nearest'})
      this.onChange(this)
    }
    snapshot() {return {kind:this.kind,pending:this.pending,index:this.index,result:this.result,states:this.states?.map(s=>({...s,selected:[...s.selected]}))}}
  }
  window.TuiChoicePreview=ChoicePreview
})()
