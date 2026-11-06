// Component-neutral host for custom, isolated documents.
async function mountComponentPanel(frameId, html, actions) {
    const frame=document.getElementById(frameId);if(!frame)return;
    let port=null,disposed=false;const pending=new Set();
    function theme(){if(!port)return;const style=getComputedStyle(document.documentElement),values={};for(const [a,b]of Object.entries({'--bg':'--canvas','--card':'--card','--fg':'--text','--muted':'--muted','--line':'--line','--accent':'--accent'})){const v=style.getPropertyValue(b).trim();if(v)values[a]=v;}port.postMessage({type:'theme',values});}
    function ready(e){
        if(disposed||port||e.source!==frame.contentWindow||e.data?.type!=='rpi-component-ready'||e.data.protocol!==1)return;
        const channel=new MessageChannel();port=channel.port1;
        port.onmessage=({data})=>{
            if(disposed||!data||data.type!=='call'||typeof data.id!=='string'||data.id.length>64||!/^[a-zA-Z0-9_-]+$/.test(data.id))return;
            const fail=error=>port?.postMessage({type:'result',id:data.id,error});if(pending.has(data.id))return;
            if(pending.size>=8)return fail('Too many component requests');
            if(!actions.includes(data.action))return fail('Action not declared');
            if(JSON.stringify(data.arguments??null).length>16384)return fail('Component request too large');
            pending.add(data.id);dioxus.send({id:data.id,action:data.action,arguments:data.arguments??null});
        };
        port.start();frame.contentWindow.postMessage({type:'rpi-component-init',protocol:1},'*',[channel.port2]);theme();
    }
    const observer=new MutationObserver(theme);observer.observe(document.documentElement,{attributes:true,attributeFilter:['class','style','data-theme','data-mode']});
    const removal=new MutationObserver(()=>{if(!frame.isConnected)dispose();});removal.observe(document.body,{childList:true,subtree:true});
    function dispose(){if(disposed)return;disposed=true;window.removeEventListener('message',ready);observer.disconnect();removal.disconnect();port?.close();pending.clear();}
    window.addEventListener('message',ready);
    const csp="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'";
    frame.srcdoc='<!doctype html><meta http-equiv="Content-Security-Policy" content="'+csp+'">'+html.replace(/<!doctype[^>]*>/ig,'');
    try{while(!disposed){const response=await dioxus.recv();if(disposed||!response||!pending.delete(response.id))continue;port?.postMessage({type:'result',...response});window.dispatchEvent(new CustomEvent('rpi-component-state-changed'));}}
    finally{dispose();}
}
