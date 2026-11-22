import {useCallback,useState,useSyncExternalStore} from 'react'
import {
  addAttachments, attachmentsOf, clearAttachments, removeAttachment, subscribeAttachments,
  type DraftAttachment,
} from '../composerAttachments.ts'
export interface AttachmentRef {attachmentId:string;name:string;bytes:number;mediaType?:string;width?:number;height?:number}
export interface AttachmentSubmission {files:readonly File[];signal:AbortSignal;requestId:string;onProgress:(label:string)=>void}
/**
 * The files a session has picked but not sent. They belong to the session, not
 * to the box that shows them: leaving to read another chat (or another page)
 * and coming back finds the picture still attached, and going elsewhere starts
 * clean. Previews are released when a file is removed or the message is sent.
 */
export function useAttachmentDraft(key:string,extensions?:readonly string[]) {
  const items=useSyncExternalStore(subscribeAttachments,()=>attachmentsOf(key),()=>attachmentsOf(key))
  const [error,setError]=useState('')
  const add=useCallback((files:Iterable<File>)=>{setError(addAttachments(key,files,extensions))},[key,extensions])
  const remove=useCallback((id:string)=>{removeAttachment(key,id)},[key])
  /** Forget a session's files; a send that ends after a switch forgets the sender's. */
  const clear=useCallback((from:string=key)=>{clearAttachments(from);if(from===key)setError('')},[key])
  return {items,add,remove,clear,error}
}
export function AttachmentRail({items,onRemove,disabled=false}:{items:readonly DraftAttachment[];onRemove:(id:string)=>void;disabled?:boolean}){
 return <div style={{display:'flex',flexWrap:'wrap',gap:8,padding:items.length?'8px 12px':0}}>{items.map(item=><div key={item.id} style={{display:'flex',alignItems:'center',gap:8,maxWidth:240,padding:8,border:'1px solid #dce1e7',borderRadius:10,background:'#f8fafc',color:'#273444'}}>
 {item.preview?<img src={item.preview} alt={item.file.name} style={{width:56,height:44,objectFit:'cover',borderRadius:6}}/>:<span aria-hidden>📎</span>}
 <span style={{fontSize:12,overflow:'hidden',textOverflow:'ellipsis',whiteSpace:'nowrap'}}>{item.file.name}</span><button type='button' aria-label={`移除 ${item.file.name}`} onClick={()=>onRemove(item.id)} disabled={disabled} style={{border:0,background:'transparent',cursor:'pointer'}}>×</button>
 </div>)}</div>
}
