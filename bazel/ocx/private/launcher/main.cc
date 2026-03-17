#include <cstdio>
#include <string>
#include <unordered_map>
#include <vector>
#include <cstring>
#include "rules_cc/cc/runfiles/runfiles.h"

#ifdef _WIN32
#include <process.h>
#else
#include <unistd.h>
#endif

using rules_cc::cc::runfiles::Runfiles;

const char* env_vars[] = ENV;


static const char** build_merged_envp(char *envp[], const std::string& layer_dir)
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
    const char** merged_envp = new const char*[merged_env.size() + 1];
    size_t i = 0;
    for (auto& [key, value] : merged_env)
    {
        std::string entry = key + "=" + value;
        char* buffer = new char[entry.size() + 1];
        memcpy(buffer, entry.c_str(), entry.size() + 1);
        merged_envp[i++] = buffer;
    }
    merged_envp[i] = nullptr;
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
    auto bin_path_in_layer_length = bin_path.length() - bin_path.find("/layer/") - 7;
    std::string layer_dir = path.substr(0, path.length() - bin_path_in_layer_length);

    const char** merged_envp = build_merged_envp(envp, layer_dir);

#ifdef _WIN32
    int res = _execve(path.c_str(), argv, (char *const *)  merged_envp);
#else
    int res = execve(path.c_str(), argv, (char *const *) merged_envp);
#endif

    if (res == -1)
    {
        fprintf(stderr, "ERROR: failed to execute '%s'\n", path.c_str());
        return 1;
    }
    return 0;
}