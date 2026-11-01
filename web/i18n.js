'use strict';
window.I18n={locale:'en-US',dictionary:{},onChange:null,t(key,values={}){
  let text=this.dictionary[key]??key;
  for(const [name,value]of Object.entries(values))text=text.replaceAll('{'+name+'}',String(value));
  return text;
}};
(()=>{
  const originals=new WeakMap(),attributes=new WeakMap();
  function translate(root){
    if(root.nodeType===Node.TEXT_NODE){
      if(root.parentElement?.closest('script,style,pre,code,textarea,#language-select'))return;
      if(!originals.has(root))originals.set(root,root.data);
      const original=originals.get(root),trimmed=original.trim();
      if(trimmed&&I18n.dictionary[trimmed]){const text=original.replace(trimmed,I18n.t(trimmed));if(root.data!==text)root.data=text;}
      else if(root.data!==original)root.data=original;
      return;
    }
    if(root.nodeType!==Node.ELEMENT_NODE&&root.nodeType!==Node.DOCUMENT_NODE)return;
    if(root.nodeType===Node.ELEMENT_NODE){
      if(!attributes.has(root)){const data={};for(const key of ['placeholder','title','aria-label'])if(root.hasAttribute(key))data[key]=root.getAttribute(key);attributes.set(root,data);}
      for(const [key,value]of Object.entries(attributes.get(root)))root.setAttribute(key,I18n.t(value));
    }
    for(const child of root.childNodes)translate(child);
  }
  const normalize=value=>value==='en'||value==='en-US'?'en-US':value==='zh-CN'?'zh-CN':null;
  async function select(locale,updateUrl=false){
    locale=normalize(locale)||'en-US';
    const response=await fetch('/locales/'+(locale==='en-US'?'en':locale)+'.json',{cache:'no-cache'});
    if(!response.ok)throw new Error('Cannot load language');
    I18n.dictionary=await response.json();I18n.locale=locale;document.documentElement.lang=locale;
    localStorage.setItem('furbo-language',locale);document.getElementById('language-select').value=locale;
    if(updateUrl){const url=new URL(location.href);url.searchParams.delete('lang');history.replaceState({},'',url.pathname+url.search+url.hash);}
    translate(document);await I18n.onChange?.();
  }
  document.getElementById('language-select').addEventListener('change',event=>select(event.target.value,true).catch(()=>{}));
  const observer=new MutationObserver(changes=>{for(const change of changes)for(const added of change.addedNodes)translate(added);});
  observer.observe(document.body,{childList:true,subtree:true});
  I18n.fromUrl=()=>{const locale=normalize(new URLSearchParams(location.search).get('lang'));return locale&&locale!==I18n.locale?select(locale):Promise.resolve();};
  I18n.ready=select(normalize(new URLSearchParams(location.search).get('lang'))||normalize(localStorage.getItem('furbo-language'))||((navigator.language||'').toLowerCase().startsWith('zh')?'zh-CN':'en-US'));
})();
