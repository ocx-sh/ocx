#include <cstdio>
#include <string>
#include <unordered_map>
#include <cstring>
#include <vector>
#include "rules_cc/cc/runfiles/runfiles.h"

#ifdef _WIN32
#include <Windows.h>
#else
#include <unistd.h>
#endif

using rules_cc::cc::runfiles::Runfiles;

const char* env_vars[] = ENV;

static std::vector<std::string> build_merged_envp(char *envp[], const std::string& layer_dir)
{
    std::unordered_map<std::string, std::string> merged_env;
    for (char** env = envp; env && *env; ++env)
    {
        std::string current = *env;
        size_t separator = current.find('=');
        if (separator == std::string::npos || separator == 0)
        {
            continue;
        }

        std::string key = current.substr(0, separator);
#ifdef _WIN32
        for (auto& c : key) c = toupper((unsigned char)c);
#endif
        merged_env[key] = current.substr(separator + 1);
    }

    for (size_t i = 0; i < sizeof(env_vars) / sizeof(env_vars[0]); ++i)
    {
        std::string current = env_vars[i];
        size_t separator = current.find('=');
        if (separator == std::string::npos || separator == 0)
        {
            continue;
        }

        std::string key = current.substr(0, separator);
#ifdef _WIN32
        for (auto& c : key) c = toupper((unsigned char)c);
#endif
        std::string value = current.substr(separator + 1);

        // Replace ${installPath} with the actual layer directory path.
        size_t install_pos = value.find("${installPath}");
        if (install_pos != std::string::npos)
        {
            value.replace(install_pos, 14, layer_dir);
        }

        // If the variable is PATH, append the existing PATH value from envp to it.
        if (key == "PATH" && merged_env.count("PATH"))
        {
        #ifdef _WIN32
            const char path_separator = ';';
        #else
            const char path_separator = ':';
        #endif
            value = value + path_separator + merged_env[key];
        }
        merged_env[key] = value;
    }

    // Convert the merged environment map back to an array of C strings.
    std::vector<std::string> merged_envp;
    merged_envp.reserve(merged_env.size());
    for (auto& [key, value] : merged_env)
    {
        merged_envp.push_back(key + "=" + value);
    }
    return merged_envp;
}

int main(int argc, char *argv[], char *envp[])
{
    (void)argc;

    std::string error;
    // Resolve the path to the executable in the runfiles structure
    auto runfiles = std::unique_ptr<Runfiles>(
        Runfiles::Create(argv[0], BAZEL_CURRENT_REPOSITORY, &error));
    if (!runfiles)
    {
        fprintf(stderr, "ERROR: %s\n", error.c_str());
        return 1;
    }
    std::string bin_path = BIN_PATH;
    std::string path = runfiles->Rlocation(bin_path);
    // Resolve the image layer root directory in the runfiles structure
    auto layer_dir_pos =  bin_path.find("/layer/");
    if (layer_dir_pos == std::string::npos) {
        fprintf(stderr, "Can't resolve layer root dir");
        return 1;
    }
    auto bin_path_in_layer_length = bin_path.length() - layer_dir_pos - 7;
    std::string layer_dir = path.substr(0, path.length() - bin_path_in_layer_length);
    std::vector<std::string> merged_envp = build_merged_envp(envp, layer_dir);

#ifdef _WIN32
    // Build command line
    std::string cmdline = "\"" + path + "\"";
    for (int i = 1; argv[i] != nullptr; ++i) {
        cmdline += " \"";
        cmdline += argv[i];
        cmdline += "\"";
    }

    // Build environment block (double-null-terminated)
    size_t env_block_len = 0;
    for (const auto& env : merged_envp) {
        env_block_len += env.size() + 1;
    }
    env_block_len += 1; // final null
    char* env_block = new char[env_block_len];
    char* p = env_block;
    for (const auto& env : merged_envp) {
        size_t len = env.size();
        memcpy(p, env.c_str(), len);
        p += len;
        *p++ = '\0';
    }
    *p = '\0';

    STARTUPINFOA si;
    PROCESS_INFORMATION pi;
    ZeroMemory(&si, sizeof(si));
    si.cb = sizeof(si);
    ZeroMemory(&pi, sizeof(pi));

    BOOL success = CreateProcessA(
        path.c_str(),
        (LPSTR)cmdline.c_str(),
        NULL,
        NULL,
        FALSE,
        0,
        env_block,
        NULL,
        &si,
        &pi
    );
    if (!success) {
        fprintf(stderr, "ERROR: failed to execute '%s' (CreateProcess failed)\n", path.c_str());
        delete[] env_block;
        return 1;
    }
    // Wait for process to finish
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD exit_code = 0;
    GetExitCodeProcess(pi.hProcess, &exit_code);
    CloseHandle(pi.hProcess);
    CloseHandle(pi.hThread);
    delete[] env_block;
    return (int)exit_code;
#else
    // Build char* array for execve
    std::vector<char*> envp_cstrs;
    envp_cstrs.reserve(merged_envp.size() + 1);
    for (const auto& env : merged_envp) {
        envp_cstrs.push_back(const_cast<char*>(env.c_str()));
    }
    envp_cstrs.push_back(nullptr);
    int res = execve(path.c_str(), argv, envp_cstrs.data());
    if (res == -1)
    {
        fprintf(stderr, "ERROR: failed to execute '%s'\n", path.c_str());
        return 1;
    }
    return 0;
#endif
}