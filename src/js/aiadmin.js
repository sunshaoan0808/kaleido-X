/**
 * src/js/aiadmin.js — AI 供应商与用量管理 真 ESM 模块（P1-3 S2.13；原 _aiadmin-part.js）。
 * 出边仅 tabs 切页时的守卫调用，经 converted[] import 恒真化，闭包零编辑。
 * 顶层副作用（DOMContentLoaded init / window.P5Ai*·MoaLoad* 发布）原样保留。
 */
import { showToast } from './toast.js';
import { showConfirm } from './dialog.js';

;/* P5: AI 供应商与用量管理 (injected) */
try{document.documentElement.setAttribute("data-p5diag","h="+typeof h+" Co="+(typeof Co)+" st="+typeof window.__kaleidoTabs.switchTab+" api="+typeof P5Api)}catch(e){document.documentElement.setAttribute("data-p5diag","err:"+e.message)};var P5AiProviders=[],P5AiPid="",P5AiPName="";
function P5Esc(v){return String(v==null?"":v).replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;")}
async function P5Api(path,opts){opts=opts||{};var t=localStorage.getItem("kaleido_token")||localStorage.getItem("token")||"";var heads=Object.assign({"Content-Type":"application/json"},opts.headers||{});if(t){heads.Authorization="Bearer "+t;heads["X-Mobile-Token"]=t}var r=await fetch(path,Object.assign({},opts,{headers:heads,cache:"no-store"}));if(!r.ok){var msg=r.statusText;try{var j=await r.json();msg=j&&(j.error||j.message)||JSON.stringify(j)}catch(e){try{msg=await r.text()}catch(_){}}var er=new Error(msg||("HTTP "+r.status));er.status=r.status;throw er}if(r.status===204)return null;var ct=r.headers.get("content-type")||"";return ct.indexOf("application/json")>=0?r.json():r.text()}
async function P5AiLoad(){
  var ub=document.getElementById("aiadm-usage"),box=document.getElementById("aiadm-providers");if(!box)return;
  try{var r=await P5Api("/api/v1/ai/providers");P5AiProviders=(r&&r.providers)||[]}catch(e){P5AiProviders=[]}
  try{var u=await P5Api("/api/v1/ai/usage?days=7");
    if(ub)ub.innerHTML="📊 近"+(u&&u.days?u.days:7)+"天：调用 <b>"+(u?u.total_calls:0)+"</b> 次 · 输入 "+(u?u.total_input_tokens:0)+" tok · 输出 "+(u?u.total_output_tokens:0)+" tok";
  }catch(e){if(ub)ub.innerHTML="📊 用量统计暂不可用"}
  if(!P5AiProviders.length){box.innerHTML='<p class="muted sm">暂无供应商。点击「＋ 新建供应商」添加第一个 OpenAI 兼容端点。</p>';return}
  var html='<div class="row wrap gap-sm">';
  P5AiProviders.forEach(function(p){
    var key=P5Esc(p.key_hint||(p.configured?"已配置":"未配置"));
    var actBadge=p.active?'<span class="badge badge-active" style="margin-left:6px">当前</span>':'';
    html+='<div class="settings card"'+(p.active?' card-active':'')+'" style="min-width:300px;flex:1 1 320px">'+
      '<div class="row between"><b>'+P5Esc(p.name)+actBadge+'</b><span class="muted sm">'+P5Esc(p.protocol)+' · '+P5Esc(p.status)+'</span></div>'+
      '<div class="muted sm" style="margin:4px 0;word-break:break-all">'+P5Esc(p.base_url)+'</div>'+
      '<div class="muted sm">Key: '+key+' · RPM '+(p.rpm_limit!=null?p.rpm_limit:"-")+' · 并发 '+(p.concurrency_limit!=null?p.concurrency_limit:"-")+' · 最大输出 '+(p.max_tokens!=null?p.max_tokens:"-")+' tok</div>'+
      (p.last_error?'<div class="muted sm" style="color:#e07070">上次错误：'+P5Esc(p.last_error)+'</div>':'')+
      '<div class="row gap-sm" style="margin-top:8px">'+
        (p.active?'':'<button type="button" class="sm" data-activate="'+p.id+'">设为当前</button>')+
        '<button type="button" class="sm" onclick="P5AiModels(\''+p.id+'\',\''+P5Esc(p.name).replace(/'/g,"\\'")+'\')">模型</button>'+
        '<button type="button" class="sm ghost" onclick="P5AiEdit(\''+p.id+'\')">编辑</button>'+
        '<button type="button" class="sm ghost danger" onclick="P5AiDel(\''+p.id+'\')">删除</button>'+
      '</div></div>';
  });
  box.innerHTML=html+'</div>';
}
function P5AiEdit(id){
  var p=null;P5AiProviders.forEach(function(x){if(x.id===id)p=x});
  var f=document.getElementById("aiadm-edit");if(!f)return;
  document.getElementById("aiadm-edit-title").textContent=p?"编辑供应商："+p.name:"新建供应商";
  document.getElementById("aiadm-f-id").value=p?id:"";
  document.getElementById("aiadm-f-name").value=p?p.name:"";
  document.getElementById("aiadm-f-protocol").value=(p&&p.protocol)?p.protocol:"openai";
  document.getElementById("aiadm-f-base").value=p?p.base_url:"";
  document.getElementById("aiadm-f-key").value="";
  document.getElementById("aiadm-f-rpm").value=p?(p.rpm_limit!=null?p.rpm_limit:60):60;
  document.getElementById("aiadm-f-conc").value=p?(p.concurrency_limit!=null?p.concurrency_limit:10):10;
  document.getElementById("aiadm-f-max").value=p?(p.max_tokens!=null?p.max_tokens:32000):32000;
  document.getElementById("aiadm-f-note").value=p?p.note:"";
  f.classList.remove("hidden");f.scrollIntoView({behavior:"smooth",block:"nearest"});
}
async function P5AiSave(){
  var id=document.getElementById("aiadm-f-id").value;
  var body={
    name:document.getElementById("aiadm-f-name").value.trim(),
    base_url:document.getElementById("aiadm-f-base").value.trim(),
    protocol:document.getElementById("aiadm-f-protocol").value,
    concurrency_limit:parseInt(document.getElementById("aiadm-f-conc").value||"10",10),
    rpm_limit:parseInt(document.getElementById("aiadm-f-rpm").value||"60",10),
    max_tokens:parseInt(document.getElementById("aiadm-f-max").value||"32000",10),
    note:document.getElementById("aiadm-f-note").value.trim()
  };
  var key=document.getElementById("aiadm-f-key").value.trim();
  if(key)body.api_key=key;
  if(!body.name||!body.base_url){showToast("请填写名称与 Base URL", 'warning');return}
  try{
    if(id){await P5Api("/api/v1/ai/providers/"+encodeURIComponent(id),{method:"PATCH",body:JSON.stringify(body)})}
    else{await P5Api("/api/v1/ai/providers",{method:"POST",body:JSON.stringify(body)})}
    document.getElementById("aiadm-edit").classList.add("hidden");
    P5AiLoad();
  }catch(e){showToast("保存失败: "+(e.message||e, 'error'))}
}
async function P5AiActivate(id){
  try{await P5Api("/api/v1/ai/providers/"+encodeURIComponent(id)+"/activate",{method:"POST"});showToast("已设为当前供应商");P5AiLoad()}catch(e){showToast("激活失败: "+(e.message||e), 'error')}
}
async function P5AiDel(id){
  if(!await showConfirm("删除该供应商及其全部模型？此操作不可恢复。"))return;
  try{await P5Api("/api/v1/ai/providers/"+encodeURIComponent(id),{method:"DELETE"});P5AiLoad()}catch(e){showToast("删除失败: "+(e.message||e, 'error'))}
}
async function P5AiModels(pid,pname){
  P5AiPid=pid;P5AiPName=pname||"";
  document.getElementById("aiadm-models-title").textContent="模型列表"+(pname?" · "+pname:"");
  document.getElementById("aiadm-models").classList.remove("hidden");
  var box=document.getElementById("aiadm-models-list");box.textContent="加载中…";
  try{
    var r=await P5Api("/api/v1/ai/providers/"+encodeURIComponent(pid)+"/models");
    var ms=(r&&r.models)||[];
    if(!ms.length){box.innerHTML='<p class="muted sm">暂无模型。<button type="button" class="sm" onclick="P5AiModelAdd(\''+pid+'\')">＋ 添加模型</button></p>';return}
    var html='<div class="row wrap gap-sm">';
    ms.forEach(function(m){
      html+='<div class="settings card" style="min-width:260px;flex:1 1 280px">'+
        '<div class="row between"><b>'+P5Esc(m.display_name)+'</b><span class="muted sm">'+(m.enabled?"🟢":"⭕停用")+'</span></div>'+
        '<div class="muted sm">'+P5Esc(m.model_id)+' · ctx '+(m.context_window||0)+'</div>'+
        '<div class="muted sm">用途：'+P5Esc((m.purposes||[]).join("、")||"-")+'</div>'+
        '<div class="row gap-sm" style="margin-top:8px">'+
          '<button type="button" class="sm" onclick="P5AiModelToggle(\''+m.id+'\','+(m.enabled?"true":"false")+')">'+(m.enabled?"停用":"启用")+'</button>'+
          '<button type="button" class="sm ghost danger" onclick="P5AiModelDel(\''+m.id+'\')">删除</button>'+
        '</div></div>';
    });
    box.innerHTML=html+'</div><div class="row" style="margin-top:10px"><button type="button" id="aiadm-model-add" class="ghost sm" onclick="P5AiModelAdd(\''+pid+'\')">＋ 添加模型</button></div>';
  }catch(e){box.textContent="模型加载失败: "+(e.message||e)}
}
async function P5AiModelAdd(pid){
  var dn=await showPrompt("显示名称（如 GPT-4o）:");if(!dn)return;
  var mid=await showPrompt("模型 ID（如 gpt-4o，须与供应商 API 一致）:");if(!mid)return;
  var ctx=parseInt(await showPrompt("上下文窗口（默认 128000）:", { value: "128000" })||"128000",10);
  var pur=(await showPrompt("用途（逗号分隔，默认 chat）:", { value: "chat" })||"chat").split(/[,，]/).map(function(x){return x.trim()}).filter(Boolean);
  try{await P5Api("/api/v1/ai/providers/"+encodeURIComponent(pid)+"/models",{method:"POST",body:JSON.stringify({display_name:dn,model_id:mid,purposes:pur,context_window:ctx,thinking_enabled:true,enabled:true,note:""})});P5AiModels(P5AiPid,P5AiPName)}catch(e){showToast("添加失败: "+(e.message||e, 'error'))}
}
async function P5AiModelToggle(id,enabled){
  try{await P5Api("/api/v1/ai/models/"+encodeURIComponent(id),{method:"PATCH",body:JSON.stringify({enabled:!enabled})});P5AiModels(P5AiPid,P5AiPName)}catch(e){showToast("操作失败: "+(e.message||e, 'error'))}
}
async function P5AiModelDel(id){
  if(!await showConfirm("删除该模型？"))return;
  try{await P5Api("/api/v1/ai/models/"+encodeURIComponent(id),{method:"DELETE"});P5AiModels(P5AiPid,P5AiPName)}catch(e){showToast("删除失败: "+(e.message||e, 'error'))}
}
function P5AiInit(){
  var nb=document.getElementById("aiadm-new"),sv=document.getElementById("aiadm-save"),cl=document.getElementById("aiadm-cancel"),bk=document.getElementById("aiadm-models-back");
  if(nb)nb.addEventListener("click",function(){P5AiEdit("")});
  if(sv)sv.addEventListener("click",P5AiSave);
  if(cl)cl.addEventListener("click",function(){document.getElementById("aiadm-edit").classList.add("hidden")});
  if(bk)bk.addEventListener("click",function(){document.getElementById("aiadm-models").classList.add("hidden")});
  document.addEventListener("click",function(ev){
    var ab=ev.target&&ev.target.closest?ev.target.closest("[data-activate]"):null;
    if(ab){P5AiActivate(ab.getAttribute("data-activate"));return}
    var b=ev.target&&ev.target.closest?ev.target.closest('[data-tab="aiadmin"]'):null;
    if(b)setTimeout(function(){
      var panel=document.getElementById("tab-aiadmin");
      if(panel){document.querySelectorAll(".tab-panel").forEach(function(p){p.classList.add("hidden");p.setAttribute("aria-hidden","true")});panel.classList.remove("hidden");panel.setAttribute("aria-hidden","false")}
      document.querySelectorAll(".tab").forEach(function(a){var r=a.dataset.tab==="aiadmin";a.classList.toggle("active",r);if(a.tagName==="BUTTON"){if(r)a.setAttribute("aria-current","page");else a.removeAttribute("aria-current")}else{a.setAttribute("aria-selected",r?"true":"false");a.setAttribute("tabindex",r?"0":"-1")}});
      P5AiLoad();
    },90);
  });
}
try{typeof Co!=="undefined"&&Co.add("aiadmin")}catch(e){}
if(document.readyState==="loading"){document.addEventListener("DOMContentLoaded",P5AiInit)}else{P5AiInit()}
try{window.P5AiActivate=P5AiActivate;window.P5AiEdit=P5AiEdit;window.P5AiDel=P5AiDel;window.P5AiModels=P5AiModels;window.P5AiModelAdd=P5AiModelAdd;window.P5AiModelDel=P5AiModelDel;window.P5AiModelToggle=P5AiModelToggle;window.P5AiSave=P5AiSave;window.P5AiLoad=P5AiLoad;window.P5AiInit=P5AiInit}catch(e){}

/* ===== exports consumed by remaining closure parts (Mechanism Y) ===== */
export { P5AiLoad };
