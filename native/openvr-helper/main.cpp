// Read-only OpenVR Background client. No scene launch, action sets, or overlays.
#include <openvr.h>
#include <cmath>
#include <cstdint>
#include <iostream>
#include <sstream>
#include <string>

static std::string quoted(const std::string &s) {
    std::ostringstream out;
    out << '"';
    for (unsigned char c : s) {
        if (c == '"' || c == '\\') out << '\\' << c;
        else if (c < 32) out << ' ';
        else out << c;
    }
    out << '"';
    return out.str();
}

static vr::HmdMatrix34_t multiply(const vr::HmdMatrix34_t &a, const vr::HmdMatrix34_t &b) {
    vr::HmdMatrix34_t result{};
    for (int r = 0; r < 3; ++r) {
        for (int c = 0; c < 4; ++c) {
            for (int k = 0; k < 3; ++k) result.m[r][c] += a.m[r][k] * b.m[k][c];
        }
        result.m[r][3] += a.m[r][3];
    }
    return result;
}

static void pose_json(std::ostream &out, vr::IVRSystem *system, vr::IVRRenderModels *models,
                      const vr::TrackedDevicePose_t *poses, vr::TrackedDeviceIndex_t index, bool hand) {
    bool valid = index < vr::k_unMaxTrackedDeviceCount && poses[index].bDeviceIsConnected && poses[index].bPoseIsValid;
    auto transform = valid ? poses[index].mDeviceToAbsoluteTracking : vr::HmdMatrix34_t{};
    bool grip = false;
    if (valid && hand && models) {
        char model[vr::k_unMaxPropertyStringSize]{};
        vr::ETrackedPropertyError error;
        system->GetStringTrackedDeviceProperty(index, vr::Prop_RenderModelName_String, model, sizeof(model), &error);
        vr::VRControllerState_t controller{};
        system->GetControllerState(index, &controller, sizeof(controller));
        vr::RenderModel_ControllerMode_State_t mode{};
        vr::RenderModel_ComponentState_t component{};
        // Index exposes the OpenXR grip origin as "grip"; "handgrip" is
        // a different legacy origin. Prefer the explicit OpenXR component,
        // then grip, and use handgrip only for older drivers.
        if (error == vr::TrackedProp_Success && (models->GetComponentState(model,
                vr::k_pch_Controller_Component_OpenXR_Grip, &controller, &mode, &component) ||
                models->GetComponentState(model, "grip", &controller, &mode, &component) ||
                models->GetComponentState(model, vr::k_pch_Controller_Component_HandGrip, &controller, &mode, &component))) {
            transform = multiply(transform, component.mTrackingToComponentLocal);
            grip = true;
        }
    }
    for (auto &row : transform.m) for (float value : row) valid = valid && std::isfinite(value);
    out << "{\"valid\":" << (valid ? "true" : "false") << ",\"index\":" << index
        << ",\"grip\":" << (grip ? "true" : "false") << ",\"matrix\":[";
    for (int r = 0; r < 3; ++r) for (int c = 0; c < 4; ++c) {
        if (r || c) out << ',';
        out << (valid ? transform.m[r][c] : 0.0f);
    }
    out << "]}";
}

int main() {
    vr::EVRInitError error = vr::VRInitError_None;
    auto system = vr::VR_Init(&error, vr::VRApplication_Background);
    if (!system) {
        std::cout << "PRIM_OPENVR {\"error\":" << quoted(vr::VR_GetVRInitErrorAsEnglishDescription(error)) << "}" << std::endl;
        return 1;
    }
    auto apps = vr::VRApplications();
    auto models = vr::VRRenderModels();
    if (!apps) { vr::VR_Shutdown(); return 2; }
    uint64_t epoch = 0;
    std::string line;
    // One response per request bounds the pipe backlog even if Prim stops drawing.
    // Closing Prim's stdin pipe also releases the helper on parent termination.
    while (std::getline(std::cin, line)) {
        if (line == "quit") break;
        std::istringstream request(line);
        std::string command;
        uint64_t sequence;
        if (!(request >> command >> sequence) || command != "poll") continue;
        vr::VREvent_t event{};
        bool quitting = false;
        while (system->PollNextEvent(&event, sizeof(event))) {
            if (event.eventType == vr::VREvent_Quit) quitting = true;
            if (event.eventType == vr::VREvent_ChaperoneUniverseHasChanged ||
                event.eventType == vr::VREvent_SeatedZeroPoseReset ||
                event.eventType == vr::VREvent_ChaperoneDataHasChanged) ++epoch;
        }
        if (quitting) {
            std::cout << "PRIM_OPENVR {\"error\":\"SteamVR stopped\"}" << std::endl;
            break;
        }
        auto pid = apps->GetCurrentSceneProcessId();
        char key[vr::k_unMaxApplicationKeyLength]{}, starting[vr::k_unMaxApplicationKeyLength]{}, name[512]{};
        auto key_error = pid ? apps->GetApplicationKeyByProcessId(pid, key, sizeof(key)) : vr::VRApplicationError_NoApplication;
        auto starting_error = apps->GetStartingApplication(starting, sizeof(starting));
        vr::EVRApplicationError name_error = vr::VRApplicationError_UnknownApplication;
        if (key_error == vr::VRApplicationError_None)
            apps->GetApplicationPropertyString(key, vr::VRApplicationProperty_Name_String, name, sizeof(name), &name_error);
        vr::TrackedDevicePose_t poses[vr::k_unMaxTrackedDeviceCount]{};
        system->GetDeviceToAbsoluteTrackingPose(vr::TrackingUniverseStanding, 0.0f, poses, vr::k_unMaxTrackedDeviceCount);
        std::ostringstream out;
        out.precision(8);
        auto scene_state = apps->GetSceneApplicationState();
        if (apps->GetCurrentSceneProcessId() != pid) scene_state = vr::EVRSceneApplicationState_Starting;
        out << "PRIM_OPENVR {\"seq\":" << sequence << ",\"epoch\":" << epoch
            << ",\"scene_pid\":" << pid << ",\"scene_state\":" << scene_state
            << ",\"key\":" << quoted(key) << ",\"key_error\":" << key_error
            << ",\"starting\":" << quoted(starting) << ",\"starting_error\":" << starting_error
            << ",\"app\":" << quoted(name_error == vr::VRApplicationError_None ? name : key) << ",\"poses\":[";
        pose_json(out, system, models, poses, vr::k_unTrackedDeviceIndex_Hmd, false);
        for (auto role : {vr::TrackedControllerRole_LeftHand, vr::TrackedControllerRole_RightHand}) {
            out << ',';
            pose_json(out, system, models, poses, system->GetTrackedDeviceIndexForControllerRole(role), true);
        }
        out << "]}";
        std::cout << out.str() << std::endl;
        if (!std::cout) break;
    }
    vr::VR_Shutdown();
    return 0;
}
