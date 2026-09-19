// Disposable ownership probe. The scene/headless modes may replace a running VR game.
#include <openvr/openvr.h>
#include <openxr/openxr.h>
#include <cstdio>
#include <cstring>
#include <thread>
#include <chrono>
#include <unistd.h>
#include <initializer_list>
int main(int argc,char**argv) {
 setbuf(stdout,nullptr);
 bool requirements=argc>1 && !strcmp(argv[1],"requirements");
 bool headless=argc>1 && (!strcmp(argv[1],"headless") || requirements);
 bool scene=argc>1 && !strcmp(argv[1],"scene");
 printf("pid=%d mode=%s\n",getpid(),headless?"headless":scene?"scene":"background");
 if(headless){
  const char*exts[]={"XR_MND_headless","XR_KHR_vulkan_enable"}; XrInstanceCreateInfo ci{XR_TYPE_INSTANCE_CREATE_INFO};
  strcpy(ci.applicationInfo.applicationName,"Prim isolated headless probe");
  ci.applicationInfo.apiVersion=XR_MAKE_VERSION(1,0,0); ci.enabledExtensionCount=2;ci.enabledExtensionNames=exts;
  XrInstance inst; auto r=xrCreateInstance(&ci,&inst);printf("create_instance=%d\n",r);if(r<0)return 1;
  XrSystemGetInfo gi{XR_TYPE_SYSTEM_GET_INFO};gi.formFactor=XR_FORM_FACTOR_HEAD_MOUNTED_DISPLAY;
  XrSystemId id; r=xrGetSystem(inst,&gi,&id); printf("get_system=%d\n",r);if(r<0){xrDestroyInstance(inst);return 2;}
  if(requirements){
   for(auto name: {"xrGetVulkanInstanceExtensionsKHR","xrGetVulkanDeviceExtensionsKHR"}){
    PFN_xrVoidFunction ptr = nullptr;
    if (XR_FAILED(xrGetInstanceProcAddr(inst,name,&ptr)) || !ptr) {
     fprintf(stderr,"Missing runtime function: %s\n",name);
     xrDestroyInstance(inst);return 6;
    }
    using Fn=XrResult(*)(XrInstance,XrSystemId,uint32_t,uint32_t*,char*);
    uint32_t size=0;
    if (XR_FAILED(((Fn)ptr)(inst,id,0,&size,nullptr)) || size > 4096) {
     xrDestroyInstance(inst);return 6;
    }
    char buffer[4096]={0};r=((Fn)ptr)(inst,id,sizeof(buffer),&size,buffer);
    printf("%s result=%d %s\n",name,r,buffer);
    if (XR_FAILED(r)) { xrDestroyInstance(inst);return 6; }
   }
   xrDestroyInstance(inst);return 0;
  }
  XrSessionCreateInfo si{XR_TYPE_SESSION_CREATE_INFO};si.systemId=id;
  XrSession s; r=xrCreateSession(inst,&si,&s);printf("create_session=%d\n",r);
  if(r>=0){std::this_thread::sleep_for(std::chrono::seconds(2));xrDestroySession(s);}
  xrDestroyInstance(inst);return r<0?3:0;
 }
 vr::EVRInitError err;
 auto sys=vr::VR_Init(&err,scene?vr::VRApplication_Scene:vr::VRApplication_Background);
 printf("init=%d (%s)\n",err,vr::VR_GetVRInitErrorAsEnglishDescription(err)); if(!sys)return 4;
 auto apps=vr::VRApplications();
 if (!apps) { vr::VR_Shutdown();return 6; }
 for(int i=0;i<(scene?600:3);++i){
  printf("scene_pid=%u state=%d\n",apps->GetCurrentSceneProcessId(),apps->GetSceneApplicationState());
  vr::TrackedDevicePose_t poses[vr::k_unMaxTrackedDeviceCount];
  sys->GetDeviceToAbsoluteTrackingPose(vr::TrackingUniverseStanding,0,poses,vr::k_unMaxTrackedDeviceCount);
  int valid=0;for(auto&p:poses)valid+=p.bPoseIsValid;
  printf("hmd_valid=%d valid_devices=%d\n",poses[0].bPoseIsValid,valid);
  vr::VREvent_t ev;
  while(sys->PollNextEvent(&ev,sizeof(ev))){
   if(ev.eventType==vr::VREvent_Quit){printf("QUIT_REQUESTED\n");sys->AcknowledgeQuit_Exiting();vr::VR_Shutdown();return 5;}
  }
  std::this_thread::sleep_for(std::chrono::milliseconds(100));
 }
 vr::VR_Shutdown();
}
